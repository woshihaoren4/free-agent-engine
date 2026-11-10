use crate::protocol::*;
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

// Keep reading even while the user is editing at the prompt. This also makes
// receiving cancellation-safe when the UI wins a select! branch.
struct Transport {
    outgoing:
        tokio::sync::mpsc::Sender<(Message, tokio::sync::oneshot::Sender<anyhow::Result<()>>)>,
    incoming: tokio::sync::mpsc::Receiver<anyhow::Result<ServerEnvelope>>,
    reader: tokio::task::JoinHandle<()>,
    writer: tokio::task::JoinHandle<()>,
}
impl Transport {
    fn new(socket: Socket) -> Self {
        let (mut sink, mut source) = socket.split();
        let (tx, incoming) = tokio::sync::mpsc::channel(256);
        let (pong_tx, mut pongs) = tokio::sync::mpsc::channel(8);
        let (outgoing, mut writes) = tokio::sync::mpsc::channel::<(
            Message,
            tokio::sync::oneshot::Sender<anyhow::Result<()>>,
        )>(16);
        let writer = tokio::spawn(async move {
            loop {
                tokio::select! {
                    data = pongs.recv() => {
                        let Some(data) = data else { break };
                        if sink.send(Message::Pong(data)).await.is_err() { break; }
                    }
                    write = writes.recv() => {
                        let Some((message, reply)) = write else { break };
                        let result = tokio::time::timeout(Duration::from_secs(10), sink.send(message))
                            .await.map_err(anyhow::Error::from).and_then(|r| r.map_err(Into::into));
                        let failed = result.is_err();
                        let _ = reply.send(result);
                        if failed { break; }
                    }
                }
            }
        });
        let reader = tokio::spawn(async move {
            while let Some(message) = source.next().await {
                let response = match message {
                    Ok(Message::Text(text)) => serde_json::from_str(&text).map_err(Into::into),
                    Ok(Message::Ping(data)) => {
                        // The writer runs independently of foreground UI input.
                        if pong_tx.send(data).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Ok(Message::Pong(_)) => continue,
                    Ok(_) => Err(anyhow::anyhow!("Service connection closed")),
                    Err(error) => Err(error.into()),
                };
                let failed = response.is_err();
                if tx.send(response).await.is_err() || failed {
                    break;
                }
            }
        });
        Self {
            outgoing,
            incoming,
            reader,
            writer,
        }
    }
    async fn send(&self, message: Message) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.outgoing.send((message, tx)).await?;
        rx.await?
    }
    async fn receive(&mut self) -> anyhow::Result<ServerEnvelope> {
        self.incoming
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("Service connection closed"))?
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}
pub async fn connect(home: &Path, config: &ServerConfig, route: &str) -> anyhow::Result<Socket> {
    let path = home.join(if route == "/v3/lark/session" {
        "lark.token"
    } else {
        "server.token"
    });
    let token = tokio::fs::read_to_string(&path)
        .await
        .map_err(|error| crate::server::lifecycle::runtime_file_error(&path, error))?;
    let mut request = config.endpoint(route).into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {}", token.trim()).parse()?);
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(2),
        tokio_tungstenite::connect_async_with_config(
            request,
            Some(
                tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(Some(MAX_FRAME))
                    .max_frame_size(Some(MAX_FRAME)),
            ),
            false,
        ),
    )
    .await??;
    Ok(socket)
}

async fn receive(socket: &mut Socket) -> anyhow::Result<ServerEnvelope> {
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("Service connection closed"))??;
        match message {
            Message::Text(text) => {
                let response: ServerEnvelope = serde_json::from_str(&text)?;
                anyhow::ensure!(
                    response.version == VERSION,
                    "unsupported_version: service uses protocol {}",
                    response.version
                );
                return Ok(response);
            }
            Message::Ping(data) => socket.send(Message::Pong(data)).await?,
            Message::Pong(_) => {}
            _ => anyhow::bail!("Service connection closed"),
        }
    }
}

pub async fn control(
    home: &Path,
    config: &ServerConfig,
    body: ControlMessage,
) -> anyhow::Result<ServerMessage> {
    let mut socket = connect(home, config, "/v1/control").await?;
    let request = ControlEnvelope {
        version: VERSION,
        request_id: wd_tools::uuid::v4(),
        body,
    };
    socket
        .send(Message::Text(serde_json::to_string(&request)?.into()))
        .await?;
    let response = tokio::time::timeout(Duration::from_secs(10), receive(&mut socket)).await??;
    anyhow::ensure!(
        response.instance_id == config.instance_id
            && response.request_id.as_deref() == Some(&request.request_id),
        "Service identity or request mismatch"
    );
    match response.body {
        ServerMessage::Error(error) => Err(error.into()),
        body => Ok(body),
    }
}

pub async fn hello(home: &Path, config: &ServerConfig) -> anyhow::Result<HelloInfo> {
    anyhow::ensure!(
        config.protocol_version == VERSION,
        "unsupported_version: stop the old service before upgrading"
    );
    let ServerMessage::Hello(info) = control(home, config, ControlMessage::Hello).await? else {
        anyhow::bail!("Invalid service handshake");
    };
    anyhow::ensure!(
        info.home == home
            && info.config.pid == config.pid
            && info.config.instance_id == config.instance_id
            && info.config.protocol_version == VERSION,
        "Service identity mismatch"
    );
    Ok(info)
}

pub struct Client {
    socket: Transport,
    home: PathBuf,
    config: ServerConfig,
    open: OpenSession,
    pub info: SessionInfo,
    pub stream: String,
    seq: u64,
    pending: VecDeque<ServerEnvelope>,
}
impl Client {
    pub async fn open(
        home: &Path,
        config: &ServerConfig,
        open: OpenSession,
    ) -> anyhow::Result<Self> {
        let mut socket = connect(
            home,
            config,
            if open.binding.is_some() {
                "/v3/lark/session"
            } else {
                "/v1/session"
            },
        )
        .await?;
        let request = ClientEnvelope {
            version: VERSION,
            request_id: wd_tools::uuid::v4(),
            body: ClientMessage::Open(open.clone()),
        };
        socket
            .send(Message::Text(serde_json::to_string(&request)?.into()))
            .await?;
        let response =
            tokio::time::timeout(Duration::from_secs(10), receive(&mut socket)).await??;
        anyhow::ensure!(
            response.instance_id == config.instance_id,
            "Service instance mismatch"
        );
        let info = match response.body {
            ServerMessage::Opened(info) => info,
            ServerMessage::Error(error) => return Err(error.into()),
            _ => anyhow::bail!("Expected opened response"),
        };
        let stream = response
            .stream_id
            .ok_or_else(|| anyhow::anyhow!("Missing stream ID"))?;
        let seq = open.resume.as_ref().map_or(0, |resume| resume.after_seq);
        Ok(Self {
            socket: Transport::new(socket),
            home: home.into(),
            config: config.clone(),
            open,
            info,
            stream,
            seq,
            pending: VecDeque::new(),
        })
    }
    pub async fn send(&mut self, body: ClientMessage) -> anyhow::Result<String> {
        let id = wd_tools::uuid::v4();
        let request = ClientEnvelope {
            version: VERSION,
            request_id: id.clone(),
            body,
        };
        let text = serde_json::to_string(&request)?;
        anyhow::ensure!(text.len() <= MAX_FRAME, "Input exceeds 8 MiB");
        if let Err(error) = self.socket.send(Message::Text(text.into())).await {
            let resumed = self.reconnect().await;
            anyhow::bail!(
                "Acceptance unknown: {error}; reconnect: {resumed:?}; query status before retrying"
            );
        }
        Ok(id)
    }
    pub async fn request(&mut self, body: ClientMessage) -> anyhow::Result<ServerMessage> {
        let id = self.send(body).await?;
        loop {
            // Do not consume pending events while awaiting a specific response.
            let envelope = match self.network_receive().await {
                Ok(envelope) => envelope,
                Err(error) => {
                    let resumed = self.reconnect().await;
                    anyhow::bail!(
                        "Acceptance unknown: {error}; reconnect: {resumed:?}; query status before retrying"
                    );
                }
            };
            if envelope.request_id.as_deref() == Some(&id) {
                return match envelope.body {
                    ServerMessage::Error(e) => Err(e.into()),
                    body => Ok(body),
                };
            }
            self.pending.push_back(envelope);
        }
    }
    async fn network_receive(&mut self) -> anyhow::Result<ServerEnvelope> {
        let envelope = self.socket.receive().await?;
        anyhow::ensure!(
            envelope.version == VERSION
                && envelope.instance_id == self.config.instance_id
                && envelope.stream_id.as_deref() == Some(&self.stream),
            "Session stream mismatch"
        );
        Ok(envelope)
    }
    pub async fn next(&mut self) -> anyhow::Result<ServerMessage> {
        self.next_message(true).await
    }
    pub(crate) async fn next_connected(&mut self) -> anyhow::Result<ServerMessage> {
        self.next_message(false).await
    }
    async fn next_message(&mut self, reconnect: bool) -> anyhow::Result<ServerMessage> {
        loop {
            let envelope = if let Some(envelope) = self.pending.pop_front() {
                envelope
            } else {
                match self.network_receive().await {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        if !reconnect {
                            return Err(error);
                        }
                        self.reconnect()
                            .await
                            .map_err(|e| anyhow::anyhow!("{error}; reconnect failed: {e}"))?;
                        continue;
                    }
                }
            };
            if let Some(seq) = envelope.body.seq() {
                if seq <= self.seq {
                    continue;
                }
                self.seq = seq;
            }
            match &envelope.body {
                ServerMessage::Ack { round_id } => self.info.status.round_id = round_id.clone(),
                ServerMessage::Finished { .. } => self.info.status.round_id = None,
                ServerMessage::Error(error) if error.code == "resume_gap" => {
                    if let Some(seq) = error.detail.as_ref().and_then(|d| d["latest_seq"].as_u64())
                    {
                        self.seq = seq;
                    }
                }
                _ => {}
            }
            return Ok(envelope.body);
        }
    }
    pub(crate) async fn reconnect(&mut self) -> anyhow::Result<()> {
        let mut last = None;
        for delay in [100, 200, 400, 800, 1600] {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let config = super::daemon::read_config(&self.home).map_err(|error| {
                anyhow::anyhow!("resume_expired: service discovery unavailable: {error}")
            })?;
            anyhow::ensure!(
                config.instance_id == self.config.instance_id,
                "resume_expired: service restarted; task was not rerun"
            );
            if self.open.binding.is_none() {
                hello(&self.home, &config).await?;
            }
            let mut open = self.open.clone();
            open.session_id = Some(self.info.session_id.clone());
            open.resume = Some(Resume {
                stream_id: self.stream.clone(),
                after_seq: self.seq,
            });
            match Client::open(&self.home, &config, open).await {
                Ok(client) => {
                    self.socket = client.socket;
                    self.info = client.info;
                    return Ok(());
                }
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("Reconnect failed")))
    }
}
