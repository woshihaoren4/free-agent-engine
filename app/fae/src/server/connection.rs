use crate::protocol::{MAX_FRAME, ProtocolError, ServerEnvelope, ServerMessage};
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
}
#[derive(Clone)]
pub struct Outbound {
    pub id: String,
    tx: mpsc::Sender<Queued>,
    bytes: Arc<Semaphore>,
    close: watch::Sender<bool>,
}
impl Outbound {
    pub fn send(&self, mut envelope: ServerEnvelope) -> bool {
        let Ok(mut text) = serde_json::to_string(&envelope) else {
            return false;
        };
        if text.len() > MAX_FRAME {
            envelope.body = ServerMessage::Error(ProtocolError::new(
                "resource_exhausted",
                "Response exceeds the 8 MiB frame limit",
            ));
            text = serde_json::to_string(&envelope).unwrap();
        }
        let permit = self.bytes.clone().try_acquire_many_owned(text.len() as u32);
        if let Ok(permit) = permit {
            if self
                .tx
                .try_send(Queued {
                    text,
                    _permit: permit,
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
) -> anyhow::Result<(WebSocketStream<TcpStream>, String)> {
    let route = Arc::new(std::sync::Mutex::new(String::new()));
    let selected = route.clone();
    let ws = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::accept_hdr_async_with_config(
            stream,
            move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
                let authorization = format!("Bearer {token}");
                let accepted = request
                    .headers()
                    .get("authorization")
                    .is_some_and(|v| v.as_bytes() == authorization.as_bytes())
                    && !request.headers().contains_key("origin")
                    && matches!(request.uri().path(), "/v1/control" | "/v1/session");
                if !accepted {
                    return Err(Response::builder()
                        .status(401)
                        .body(Some("unauthorized".into()))
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
    Ok((ws, path))
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
    fn outbound() -> (Outbound, mpsc::Receiver<Queued>, watch::Receiver<bool>) {
        let (tx, rx) = mpsc::channel(256);
        let (close, closed) = watch::channel(false);
        (Outbound { id: "test".into(), tx, bytes: Arc::new(Semaphore::new(MAX_FRAME)), close }, rx, closed)
    }
    #[test]
    fn slow_client_queue_is_bounded_and_signals_disconnect() {
        let (out, _rx, closed) = outbound();
        for _ in 0..256 {
            assert!(out.send(ServerEnvelope::new("instance", None, None, ServerMessage::Closed)));
        }
        assert!(!out.send(ServerEnvelope::new("instance", None, None, ServerMessage::Closed)));
        assert!(*closed.borrow());
        let (out, _rx, closed) = outbound();
        let envelope = ServerEnvelope::new("instance", None, None, ServerMessage::Error(ProtocolError::new("execution_failed", "x".repeat(MAX_FRAME / 2))));
        assert!(out.send(envelope.clone()));
        assert!(!out.send(envelope));
        assert!(*closed.borrow());
    }
}
