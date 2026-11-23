use crate::protocol::MAX_FRAME;
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::server::{ErrorResponse, Request, Response},
        protocol::WebSocketConfig,
    },
};

pub struct Queued {
    pub text: String,
    _permit: OwnedSemaphorePermit,
    written: Option<tokio::sync::oneshot::Sender<()>>,
}
#[derive(Clone)]
pub struct Outbound {
    pub id: String,
    tx: mpsc::Sender<Queued>,
    bytes: Arc<Semaphore>,
    close: watch::Sender<bool>,
}
impl Outbound {
    pub fn send(&self, envelope: fae_agent::protocol::ServerFrame) -> bool {
        self.enqueue(envelope, None)
    }
    pub async fn send_confirmed(&self, envelope: fae_agent::protocol::ServerFrame) -> bool {
        let (written, receipt) = tokio::sync::oneshot::channel();
        self.enqueue(envelope, Some(written))
            && matches!(
                tokio::time::timeout(Duration::from_secs(10), receipt).await,
                Ok(Ok(()))
            )
    }
    fn enqueue(
        &self,
        envelope: fae_agent::protocol::ServerFrame,
        written: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> bool {
        let Ok(text) = serde_json::to_string(&envelope) else {
            return false;
        };
        if text.len() > MAX_FRAME {
            self.close();
            return false;
        }
        let permit = self.bytes.clone().try_acquire_many_owned(text.len() as u32);
        if let Ok(permit) = permit {
            if self
                .tx
                .try_send(Queued {
                    text,
                    _permit: permit,
                    written,
                })
                .is_ok()
            {
                return true;
            }
        }
        let _ = self.close.send(true);
        false
    }
    pub fn close(&self) {
        let _ = self.close.send(true);
    }
}

/// Closing the handler also closes its reader and writer tasks, including when
/// the listener's JoinSet aborts the handler during shutdown.
pub struct ConnectionGuard(pub Outbound);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}

pub async fn upgrade(
    stream: TcpStream,
    token: String,
    lark_token: String,
) -> anyhow::Result<(
    WebSocketStream<TcpStream>,
    String,
    Option<crate::session_host::LarkPrincipal>,
)> {
    let route = Arc::new(std::sync::Mutex::new(String::new()));
    let selected = route.clone();
    let principal = Arc::new(std::sync::Mutex::new(None));
    let authenticated = principal.clone();
    let ws = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::accept_hdr_async_with_config(
            stream,
            move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
                let authorization = format!("Bearer {token}");
                let lark = request.uri().path() == "/v4/lark/session";
                let authorization = if lark {
                    format!("Bearer {lark_token}")
                } else {
                    authorization
                };
                let accepted = request
                    .headers()
                    .get("authorization")
                    .is_some_and(|v| v.as_bytes() == authorization.as_bytes())
                    && !request.headers().contains_key("origin")
                    && matches!(
                        request.uri().path(),
                        "/v4/control" | "/v4/session" | "/v4/lark/session"
                    );
                if !accepted {
                    return Err(Response::builder()
                        .status(401)
                        .body(Some("unauthorized".into()))
                        .unwrap());
                }
                if lark {
                    let parsed = request
                        .headers()
                        .get("x-fae-lark-principal")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| serde_json::from_str(v).ok());
                    if parsed.is_none() {
                        return Err(Response::builder()
                            .status(403)
                            .body(Some("Lark principal required".into()))
                            .unwrap());
                    }
                    *authenticated.lock().unwrap() = parsed;
                } else if request.headers().contains_key("x-fae-lark-principal") {
                    return Err(Response::builder()
                        .status(403)
                        .body(Some(
                            "Lark principal requires gateway authentication".into(),
                        ))
                        .unwrap());
                }
                *selected.lock().unwrap() = request.uri().path().into();
                Ok(response)
            },
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(MAX_FRAME))
                    .max_frame_size(Some(MAX_FRAME)),
            ),
        ),
    )
    .await??;
    let path = route.lock().unwrap().clone();
    let principal = principal.lock().unwrap().clone();
    Ok((ws, path, principal))
}

pub fn transport(
    ws: WebSocketStream<TcpStream>,
) -> (
    Outbound,
    mpsc::Receiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = mpsc::channel::<Queued>(256);
    let (incoming, input) = mpsc::channel(256);
    let (close, mut closed) = watch::channel(false);
    let outbound = Outbound {
        id: wd_tools::uuid::v4(),
        tx,
        bytes: Arc::new(Semaphore::new(MAX_FRAME)),
        close,
    };
    let writer_close = outbound.clone();
    let (pong_tx, mut pong_rx) = mpsc::channel(8);
    let writer = tokio::spawn(async move {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        loop {
            let message = tokio::select! {
                _ = closed.changed() => break,
                item = rx.recv() => match item {
                    Some(item) => {
                        if !matches!(tokio::time::timeout(Duration::from_secs(10), sink.send(Message::Text(item.text.clone().into()))).await, Ok(Ok(())))
                            { break; }
                        if let Some(written) = item.written {
                            let _ = written.send(());
                        }
                        continue;
                    }
                    None => break,
                },
                pong = pong_rx.recv() => match pong { Some(data) => Message::Pong(data), None => break },
                _ = heartbeat.tick() => Message::Ping(Vec::new().into()),
            };
            if !matches!(
                tokio::time::timeout(Duration::from_secs(10), sink.send(message)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
        writer_close.close();
    });
    let reader_close = outbound.clone();
    let task = tokio::spawn(async move {
        let mut closed = reader_close.close.subscribe();
        loop {
            let message = tokio::select! {
                _ = closed.changed() => break,
                result = tokio::time::timeout(Duration::from_secs(45), source.next()) => match result {
                    Ok(Some(Ok(message))) => message,
                    _ => break,
                },
            };
            match message {
                Message::Text(text) => {
                    if incoming.try_send(text.to_string()).is_err() {
                        break;
                    }
                }
                Message::Ping(data) => {
                    if pong_tx.try_send(data).is_err() {
                        break;
                    }
                }
                Message::Pong(_) => {}
                Message::Close(_) => break,
                _ => break,
            }
        }
        reader_close.close();
        let _ = writer.await;
    });
    (outbound, input, task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::protocol::*;
    fn frame(message: String) -> ServerFrame {
        let request = fae_agent::protocol::Message::request(
            fae_agent::RequestId::new("r").unwrap(),
            None,
            RequestPayload::EngineStatus(Empty {}),
        )
        .unwrap();
        ServerFrame::Response(
            ResponseMessage::error(&request, ResponseCode::InternalError, message).unwrap(),
        )
    }
    fn outbound() -> (Outbound, mpsc::Receiver<Queued>, watch::Receiver<bool>) {
        let (tx, rx) = mpsc::channel(256);
        let (close, closed) = watch::channel(false);
        (
            Outbound {
                id: "test".into(),
                tx,
                bytes: Arc::new(Semaphore::new(MAX_FRAME)),
                close,
            },
            rx,
            closed,
        )
    }
    #[test]
    fn slow_client_queue_is_bounded_and_signals_disconnect() {
        let (out, _rx, closed) = outbound();
        for _ in 0..256 {
            assert!(out.send(frame("closed".into())));
        }
        assert!(!out.send(frame("closed".into())));
        assert!(*closed.borrow());
        let (out, _rx, closed) = outbound();
        let envelope = frame("x".repeat(MAX_FRAME / 2));
        assert!(out.send(envelope.clone()));
        assert!(!out.send(envelope));
        assert!(*closed.borrow());
    }
}
