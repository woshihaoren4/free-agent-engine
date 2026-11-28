use crate::{
    protocol::{MAX_FRAME, ServerConfig},
    session_host::LarkPrincipal,
};
use fae_agent::{ExecutionId, RequestId, SessionId, protocol::*};
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message as WsMessage, client::IntoClientRequest},
};
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug)]
pub struct ResponseError {
    pub code: ResponseCode,
    pub message: String,
}
impl std::fmt::Display for ResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code as u16, self.message)
    }
}
impl std::error::Error for ResponseError {}

struct Transport {
    outgoing:
        tokio::sync::mpsc::Sender<(WsMessage, tokio::sync::oneshot::Sender<anyhow::Result<()>>)>,
    incoming: tokio::sync::mpsc::Receiver<anyhow::Result<ServerFrame>>,
    reader: tokio::task::JoinHandle<()>,
    writer: tokio::task::JoinHandle<()>,
}
impl Transport {
    fn new(socket: Socket) -> Self {
        let (mut sink, mut source) = socket.split();
        let (tx, incoming) = tokio::sync::mpsc::channel(256);
        let (pong_tx, mut pongs) = tokio::sync::mpsc::channel(8);
        let (outgoing, mut writes) = tokio::sync::mpsc::channel::<(
            WsMessage,
            tokio::sync::oneshot::Sender<anyhow::Result<()>>,
        )>(16);
        let writer = tokio::spawn(async move {
            loop {
                tokio::select! {
                    data = pongs.recv() => {
                        let Some(data) = data else { break };
                        if sink.send(WsMessage::Pong(data)).await.is_err() { break; }
                    }
                    write = writes.recv() => {
                        let Some((message, reply)) = write else { break };
                        let result = tokio::time::timeout(Duration::from_secs(10), sink.send(message))
                            .await.map_err(anyhow::Error::from).and_then(|r| r.map_err(Into::into));
                        let failed = result.is_err(); let _ = reply.send(result); if failed { break; }
                    }
                }
            }
        });
        let reader = tokio::spawn(async move {
            while let Some(message) = source.next().await {
                let response = match message {
                    Ok(WsMessage::Text(text)) => serde_json::from_str(&text).map_err(Into::into),
                    Ok(WsMessage::Ping(data)) => {
                        if pong_tx.send(data).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Ok(WsMessage::Pong(_)) => continue,
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
    async fn send(&self, request: &Message) -> anyhow::Result<()> {
        let text = serde_json::to_string(request)?;
        anyhow::ensure!(text.len() <= MAX_FRAME, "Input exceeds 8 MiB");
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.outgoing
            .send((WsMessage::Text(text.into()), tx))
            .await?;
        rx.await?
    }
    async fn receive(&mut self) -> anyhow::Result<ServerFrame> {
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
    connect_principal(home, config, route, None).await
}
async fn connect_principal(
    home: &Path,
    config: &ServerConfig,
    route: &str,
    principal: Option<&LarkPrincipal>,
) -> anyhow::Result<Socket> {
    let path = home.join(if route == "/api/0.1.0/lark/session" {
        "lark/lark.token"
    } else {
        "server/server.token"
    });
    let token = tokio::fs::read_to_string(&path)
        .await
        .map_err(|error| crate::server::lifecycle::runtime_file_error(&path, error))?;
    let mut request = config.endpoint(route).into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {}", token.trim()).parse()?);
    if let Some(principal) = principal {
        request.headers_mut().insert(
            "x-fae-lark-principal",
            serde_json::to_string(principal)?.parse()?,
        );
    }
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
pub async fn control(
    home: &Path,
    config: &ServerConfig,
    body: RequestPayload,
) -> anyhow::Result<ResponsePayload> {
    anyhow::ensure!(!body.event().is_session(), "Expected engine operation");
    let mut socket = Transport::new(connect(home, config, "/api/0.1.0/control").await?);
    let request = Message::request(RequestId::new(wd_tools::uuid::v4())?, None, body)?;
    socket.send(&request).await?;
    let response = tokio::time::timeout(Duration::from_secs(10), socket.receive()).await??;
    let ServerFrame::Response(response) = response else {
        anyhow::bail!("Expected engine response");
    };
    validate_response(&request, &response)?;
    response_data(&request, response)
}
pub async fn hello(home: &Path, config: &ServerConfig) -> anyhow::Result<HelloResult> {
    anyhow::ensure!(
        config.protocol_version == API_VERSION,
        "unsupported_version: stop the old service before upgrading"
    );
    let ResponsePayload::EngineHello(info) =
        control(home, config, RequestPayload::EngineHello(HelloRequest {})).await?
    else {
        anyhow::bail!("Invalid service handshake");
    };
    anyhow::ensure!(
        Path::new(&info.instance.home) == home
            && info.instance.pid == config.pid
            && info.instance.id == config.instance_id
            && info.api_version == API_VERSION,
        "Service identity mismatch"
    );
    Ok(info)
}
fn validate_response(request: &Message, response: &ResponseMessage) -> anyhow::Result<()> {
    response.validate_for(request)?;
    Ok(())
}
fn response_data(request: &Message, response: ResponseMessage) -> anyhow::Result<ResponsePayload> {
    response.validate_for(request)?;
    if response.code != ResponseCode::Success {
        return Err(ResponseError {
            code: response.code,
            message: response.msg,
        }
        .into());
    }
    response
        .decode_for(request)?
        .ok_or_else(|| anyhow::anyhow!("Missing response data"))
}
pub struct Client {
    socket: Transport,
    home: PathBuf,
    config: ServerConfig,
    principal: Option<LarkPrincipal>,
    pub info: SessionSummary,
    pub stream: String,
    pub subscription: String,
    seq: u64,
    pending: VecDeque<Message>,
    replay_gap: bool,
}
impl Client {
    pub async fn open(
        home: &Path,
        config: &ServerConfig,
        create: SessionCreateRequest,
        session_id: Option<String>,
        principal: Option<LarkPrincipal>,
        resume: Option<SubscribeRequest>,
    ) -> anyhow::Result<Self> {
        let route = if principal.is_some() {
            "/api/0.1.0/lark/session"
        } else {
            "/api/0.1.0/session"
        };
        let mut socket =
            Transport::new(connect_principal(home, config, route, principal.as_ref()).await?);
        let candidate =
            session_id.or_else(|| principal.as_ref().map(crate::lark::identity::session_id));
        let request = match &candidate {
            Some(id) => Message::request(
                RequestId::new(wd_tools::uuid::v4())?,
                Some(SessionId::new(id.clone())?),
                RequestPayload::SessionGet(Empty {}),
            )?,
            None => Message::request(
                RequestId::new(wd_tools::uuid::v4())?,
                None,
                RequestPayload::SessionCreate(create.clone()),
            )?,
        };
        socket.send(&request).await?;
        let ServerFrame::Response(response) =
            tokio::time::timeout(Duration::from_secs(10), socket.receive()).await??
        else {
            anyhow::bail!("Expected session response");
        };
        validate_response(&request, &response)?;
        // Gateway creation derives the thread identity from authenticated host metadata.
        let (request, response) =
            if response.code == ResponseCode::NotFound && principal.is_some() && resume.is_none() {
                let request = Message::request(
                    RequestId::new(wd_tools::uuid::v4())?,
                    None,
                    RequestPayload::SessionCreate(create.clone()),
                )?;
                socket.send(&request).await?;
                let ServerFrame::Response(response) = socket.receive().await? else {
                    anyhow::bail!("Expected create response");
                };
                validate_response(&request, &response)?;
                (request, response)
            } else {
                (request, response)
            };
        let info = match response_data(&request, response)? {
            ResponsePayload::SessionGet(info) | ResponsePayload::SessionCreate(info) => info,
            _ => anyhow::bail!("Expected session summary"),
        };
        if candidate.is_some() && principal.is_none() && resume.is_none() {
            anyhow::ensure!(
                info.session_config.target == create.target,
                "session_conflict: Existing Session has a different target"
            );
        }
        let mut client = Self {
            socket,
            home: home.into(),
            config: config.clone(),
            principal,
            info,
            stream: String::new(),
            subscription: String::new(),
            seq: resume.as_ref().and_then(|r| r.after_seq).unwrap_or(0),
            pending: VecDeque::new(),
            replay_gap: false,
        };
        if candidate.is_some() && client.principal.is_none() && resume.is_none() {
            if let Some(workspace) = &create.workspace {
                if workspace != &client.info.session_config.workspace.root {
                    let ResponsePayload::SessionConfigUpdate(updated) = client
                        .request(RequestPayload::SessionConfigUpdate(ConfigUpdateRequest {
                            workspace: Some(workspace.clone()),
                            ..Default::default()
                        }))
                        .await?
                    else {
                        anyhow::bail!("Expected session configuration");
                    };
                    client.info.session_config = updated;
                }
            }
        }
        let resumed = resume.is_some();
        let ResponsePayload::SessionSubscribe(subscribed) = client
            .request(RequestPayload::SessionSubscribe(resume.unwrap_or_default()))
            .await?
        else {
            anyhow::bail!("Expected subscription");
        };
        client.stream = subscribed.stream_id;
        client.subscription = subscribed.subscription_id;
        client.info = subscribed.snapshot;
        if !resumed {
            client.seq = subscribed.snapshot_seq;
        }
        Ok(client)
    }
    pub fn sid(&self) -> &str {
        self.info.session_config.session_id.as_str()
    }
    pub fn active(&self) -> Option<String> {
        self.info.active_execution.as_ref().map(ToString::to_string)
    }
    pub(crate) fn take_replay_gap(&mut self) -> bool {
        std::mem::take(&mut self.replay_gap)
    }
    pub async fn send(&mut self, body: RequestPayload) -> anyhow::Result<String> {
        let id = RequestId::new(wd_tools::uuid::v4())?;
        let request = Message::request(
            id.clone(),
            Some(self.info.session_config.session_id.clone()),
            body,
        )?;
        self.socket.send(&request).await?;
        Ok(id.to_string())
    }
    pub async fn request(&mut self, body: RequestPayload) -> anyhow::Result<ResponsePayload> {
        self.request_with_id(body, RequestId::new(wd_tools::uuid::v4())?)
            .await
    }
    pub async fn request_with_id(
        &mut self,
        body: RequestPayload,
        id: RequestId,
    ) -> anyhow::Result<ResponsePayload> {
        let request =
            Message::request(id, Some(self.info.session_config.session_id.clone()), body)?;
        self.socket.send(&request).await?;
        loop {
            let frame = self.socket.receive().await?;
            match frame {
                ServerFrame::Response(response)
                    if Some(&response.response_id) == request.request_id.as_ref() =>
                {
                    validate_response(&request, &response)?;
                    let data = response_data(&request, response)?;
                    match &data {
                        ResponsePayload::SessionExecutionStart(accepted) => {
                            self.info.active_execution = Some(accepted.execution_id.clone())
                        }
                        ResponsePayload::SessionGet(info) => self.info = info.clone(),
                        _ => {}
                    }
                    return Ok(data);
                }
                ServerFrame::Event(event) => {
                    self.validate_event(&event)?;
                    self.pending.push_back(event);
                }
                ServerFrame::Response(_) => {}
            }
        }
    }
    pub async fn stop_round(&mut self, round_id: Option<String>) -> anyhow::Result<bool> {
        let Some(id) = round_id else {
            return Ok(false);
        };
        let result = self
            .request(RequestPayload::SessionExecutionStop(ExecutionStopRequest {
                execution_id: ExecutionId::new(id)?,
                reason: None,
            }))
            .await;
        match result {
            Ok(ResponsePayload::SessionExecutionStop(result)) => Ok(result.accepted),
            Ok(_) => anyhow::bail!("Expected execution stop result"),
            Err(error)
                if error
                    .downcast_ref::<ResponseError>()
                    .is_some_and(|error| error.code == ResponseCode::NotFound) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    pub async fn close(&mut self) -> anyhow::Result<()> {
        self.request(RequestPayload::SessionUnsubscribe(UnsubscribeRequest {
            subscription_id: self.subscription.clone(),
        }))
        .await?;
        Ok(())
    }
    fn validate_event(&self, event: &Message) -> anyhow::Result<()> {
        anyhow::ensure!(
            event.session_id.as_ref() == Some(&self.info.session_config.session_id)
                && (self.stream.is_empty() || event.stream_id.as_deref() == Some(&self.stream)),
            "Session stream mismatch"
        );
        Ok(())
    }
    pub async fn next(&mut self) -> anyhow::Result<Message> {
        self.next_message(true).await
    }
    pub(crate) async fn next_connected(&mut self) -> anyhow::Result<Message> {
        self.next_message(false).await
    }
    async fn next_message(&mut self, reconnect: bool) -> anyhow::Result<Message> {
        loop {
            let event = if let Some(event) = self.pending.pop_front() {
                event
            } else {
                match self.socket.receive().await {
                    Ok(ServerFrame::Event(event)) => event,
                    Ok(ServerFrame::Response(_)) => continue,
                    Err(error) => {
                        if !reconnect {
                            return Err(error);
                        }
                        self.reconnect()
                            .await
                            .map_err(|e| anyhow::anyhow!("{error}; reconnect failed: {e}"))?;
                        if self.take_replay_gap() {
                            anyhow::bail!(
                                "resume_gap: output incomplete; query execution/get and conversation/list"
                            );
                        }
                        continue;
                    }
                }
            };
            self.validate_event(&event)?;
            let seq = event.seq.unwrap();
            if seq <= self.seq {
                continue;
            }
            self.seq = seq;
            if let MessagePayload::Event(EventPayload::ExecutionFinished(finished)) = &event.payload
            {
                if self.info.active_execution.as_ref() == Some(&finished.execution_id) {
                    self.info.active_execution = None;
                }
            }
            return Ok(event);
        }
    }
    pub(crate) async fn reconnect(&mut self) -> anyhow::Result<()> {
        let config = super::daemon::read_config(&self.home)?;
        anyhow::ensure!(
            config.instance_id == self.config.instance_id,
            "resume_expired: service restarted; execution was not rerun"
        );
        let route = if self.principal.is_some() {
            "/api/0.1.0/lark/session"
        } else {
            "/api/0.1.0/session"
        };
        self.socket = Transport::new(
            connect_principal(&self.home, &config, route, self.principal.as_ref()).await?,
        );
        let result = self
            .request(RequestPayload::SessionSubscribe(SubscribeRequest {
                stream_id: Some(self.stream.clone()),
                after_seq: Some(self.seq),
            }))
            .await;
        let data = match result {
            Err(error) if error.to_string().contains("resume_gap") => {
                self.replay_gap = true;
                self.pending.clear();
                let data = self
                    .request(RequestPayload::SessionSubscribe(SubscribeRequest::default()))
                    .await?;
                if let ResponsePayload::SessionSubscribe(snapshot) = &data {
                    self.seq = snapshot.snapshot_seq;
                }
                data
            }
            result => result?,
        };
        let ResponsePayload::SessionSubscribe(subscribed) = data else {
            anyhow::bail!("Expected subscription");
        };
        self.subscription = subscribed.subscription_id;
        self.info = subscribed.snapshot;
        Ok(())
    }
}
pub fn text_content(text: impl Into<String>) -> Content {
    vec![ContentBlock::Text { text: text.into() }]
}
pub fn outcome_result(outcome: fae_agent::ExecutionOutcome) -> anyhow::Result<serde_json::Value> {
    match outcome {
        fae_agent::ExecutionOutcome::Succeeded { result } => match result.as_slice() {
            [ContentBlock::Json { value }] => Ok(value.clone()),
            [ContentBlock::Text { text }] => Ok(text.clone().into()),
            _ => Ok(serde_json::to_value(result)?),
        },
        fae_agent::ExecutionOutcome::Failed { error } => {
            anyhow::bail!("{}: {}", error.code, error.message)
        }
        fae_agent::ExecutionOutcome::Cancelled { reason } => anyhow::bail!("Cancelled: {reason}"),
    }
}
