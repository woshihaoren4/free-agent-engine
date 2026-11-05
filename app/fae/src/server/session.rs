use super::{
    connection::Outbound,
    engine::build_engine,
    registry::{Key, Registry},
    replay::{Replay, Requests},
};
use crate::protocol::*;
use fae_agent::{
    CommonSession, ContextState, Ctx, Session, SessionInput, SessionOutput, SingleAgentEnv,
    SingleAgentPlanBuilder, WorkflowEnv,
};
use fae_engine::{Engine, SessionRuntime, TodoTool};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

pub enum Command {
    Attach {
        out: Outbound,
        request: String,
        resume: Option<Resume>,
    },
    Request(ClientEnvelope),
    Detach {
        lease: String,
    },
    Started {
        session: CommonSession,
        context: Option<Ctx>,
    },
    Output(SessionOutput),
    Finished(RoundOutcome),
    Stop(Option<oneshot::Sender<anyhow::Result<()>>>),
}

pub struct Actor {
    registry: Arc<Registry>,
    key: Key,
    stream: String,
    target: SessionTarget,
    info: SessionInfo,
    builder: Option<SingleAgentPlanBuilder>,
    engine: Option<Engine>,
    session: Option<CommonSession>,
    context: Option<Ctx>,
    tx: mpsc::Sender<Command>,
    out: Option<Outbound>,
    lease: String,
    round: Option<String>,
    cancel_pending: bool,
    stop: bool,
    stop_reply: Option<oneshot::Sender<anyhow::Result<()>>>,
    replay: Replay,
    requests: Requests,
    workflow_launched: bool,
}

impl Actor {
    pub fn new(
        registry: Arc<Registry>,
        key: Key,
        stream: String,
        target: SessionTarget,
        info: SessionInfo,
        builder: SingleAgentPlanBuilder,
        tx: mpsc::Sender<Command>,
    ) -> Self {
        Self {
            registry,
            key,
            stream,
            target,
            info,
            builder: Some(builder),
            engine: None,
            session: None,
            context: None,
            tx,
            out: None,
            lease: String::new(),
            round: None,
            cancel_pending: false,
            stop: false,
            stop_reply: None,
            replay: Replay::new(),
            requests: Requests::new(),
            workflow_launched: false,
        }
    }
    fn envelope(&self, request: Option<&str>, body: ServerMessage) -> ServerEnvelope {
        ServerEnvelope::new(&self.registry.instance, Some(&self.stream), request, body)
    }
    fn send(&mut self, envelope: ServerEnvelope) {
        if self.out.as_ref().is_some_and(|out| !out.send(envelope)) {
            self.out = None;
            self.registry.detach(&self.key, &self.stream, &self.lease);
        }
    }
    fn status(&self) -> SessionStatus {
        SessionStatus {
            state: if self.cancel_pending {
                "cancelling"
            } else if self.round.is_some() {
                "running"
            } else {
                "idle"
            }
            .into(),
            round_id: self.round.clone(),
            latest_seq: self.replay.seq,
        }
    }
    async fn cancel(&mut self) -> Result<(), ProtocolError> {
        if let Some(ctx) = &self.context {
            if ctx.state().current().is_terminal() {
                return Err(ProtocolError::new(
                    "round_not_active",
                    "Round is already terminal; wait for finished",
                ));
            }
            ctx.set_state(ContextState::cancelled("Cancelled by client"))
                .map_err(|e| ProtocolError::new("execution_failed", e))?;
        } else if let Some(session) = &self.session {
            session
                .cancel_active("Cancelled by client")
                .await
                .map_err(|e| ProtocolError::new("round_not_active", e))?;
        }
        self.cancel_pending = true;
        Ok(())
    }
    async fn start(
        &mut self,
        input: Option<(fae_agent::SessionInputData, Option<String>)>,
    ) -> Result<String, ProtocolError> {
        self.registry.reserve_round(&self.key)?;
        let round = wd_tools::uuid::v4();
        self.round = Some(round.clone());
        self.cancel_pending = false;
        self.context = None;
        if self.engine.is_none() {
            let loader = fae_agent::FAEWorkflowMetadataLoader::with_home_dir(&self.registry.home);
            self.engine = Some(build_engine(loader, self.builder.take().unwrap()).await);
        }
        let engine = self.engine.as_ref().unwrap().clone();
        let previous = self.session.clone();
        self.session = None;
        let target = self.target.clone();
        let user = self.info.user_id.clone();
        let session_id = self.info.session_id.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = async {
                let (session, context, mut state) = match target {
                    SessionTarget::Agent { source } => {
                        let (input, mode) =
                            input.ok_or_else(|| anyhow::anyhow!("missing agent input"))?;
                        if let Some(session) = previous {
                            session.set_mode(mode.as_deref()).await?;
                            session.call(SessionInput::NewChat(input)).await?;
                            let state = session.active_state()?;
                            tx.send(Command::Started {
                                session: session.clone(),
                                context: None,
                            })
                            .await?;
                            (session, None, state)
                        } else {
                            let (env, session) =
                                SingleAgentEnv::new_with_user_id(source, input.text, user);
                            let mut env = env
                                .with_images(input.images.unwrap_or_default())
                                .with_session_id(session_id);
                            env.mode = mode;
                            let ctx = engine.launch(env).await?;
                            let state = ctx.state();
                            tx.send(Command::Started {
                                session: session.clone(),
                                context: Some(ctx.clone()),
                            })
                            .await?;
                            (session, Some(ctx), state)
                        }
                    }
                    SessionTarget::Workflow { workflow_id, input } => {
                        let (env, session) =
                            WorkflowEnv::new_with_user_id(workflow_id, input, user);
                        let ctx = engine.launch(env).await?;
                        let state = ctx.state();
                        tx.send(Command::Started {
                            session: session.clone(),
                            context: Some(ctx.clone()),
                        })
                        .await?;
                        (session, Some(ctx), state)
                    }
                };
                // One consumer handles both streaming output and Context completion.
                loop {
                    if state.current().is_terminal() {
                        break;
                    }
                    tokio::select! {
                        biased;
                        output = session.answer() => match output? {
                            Some(output) => { tx.send(Command::Output(output)).await?; }
                            None => break,
                        },
                        changed = state.changed() => { changed?; },
                    }
                }
                session.wait_idle().await;
                for output in session.drain_outputs().await {
                    tx.send(Command::Output(output)).await?;
                }
                let outcome = match &*state.current() {
                    ContextState::Succeeded { output } => RoundOutcome::Succeeded {
                        result: output.json_snapshot()?,
                    },
                    ContextState::Failed { error } => RoundOutcome::Failed {
                        message: error.message.clone(),
                    },
                    ContextState::Cancelled { reason } => RoundOutcome::Cancelled {
                        reason: reason.to_string(),
                    },
                    _ => RoundOutcome::Failed {
                        message: "Event source closed before completion".into(),
                    },
                };
                drop(context);
                Ok::<_, anyhow::Error>(outcome)
            }
            .await;
            let outcome = result.unwrap_or_else(|e| RoundOutcome::Failed {
                message: format!("{e:#}"),
            });
            let _ = tx.send(Command::Finished(outcome)).await;
        });
        Ok(round)
    }
    async fn request(&mut self, body: ClientMessage) -> Result<ServerMessage, ProtocolError> {
        match body {
            ClientMessage::Input { input, mode } => {
                if !matches!(self.target, SessionTarget::Agent { .. }) {
                    return Err(ProtocolError::new(
                        "unsupported_operation",
                        "Workflow does not accept agent input",
                    ));
                }
                match input {
                    SessionInput::NewChat(input) => {
                        if mode.as_deref().is_some_and(|m| m != "todo" && m != "chat") {
                            return Err(ProtocolError::new(
                                "invalid_message",
                                "Unknown agent mode",
                            ));
                        }
                        validate_input(&input).await?;
                        let round = self.start(Some((input, mode))).await?;
                        Ok(ServerMessage::Ack {
                            round_id: Some(round),
                        })
                    }
                    SessionInput::Supplement(input) => {
                        if mode.is_some() {
                            return Err(ProtocolError::new(
                                "invalid_message",
                                "Supplement mode must be null",
                            ));
                        }
                        if self.round.is_none() || self.cancel_pending {
                            return Err(ProtocolError::new(
                                "round_not_active",
                                "No round accepts steering",
                            ));
                        }
                        validate_input(&input).await?;
                        let session = self.session.as_ref().ok_or_else(|| {
                            ProtocolError::new("round_not_active", "Round is still starting")
                        })?;
                        session
                            .call(SessionInput::Supplement(input))
                            .await
                            .map_err(|e| ProtocolError::new("round_not_active", e))?;
                        Ok(ServerMessage::Ack {
                            round_id: self.round.clone(),
                        })
                    }
                }
            }
            ClientMessage::Cancel { round_id } => {
                if self.round.as_deref() != Some(&round_id) {
                    return Err(ProtocolError::new(
                        "round_not_active",
                        "Round ID is not active",
                    ));
                }
                self.cancel().await?;
                Ok(ServerMessage::Ack {
                    round_id: Some(round_id),
                })
            }
            ClientMessage::Status => Ok(ServerMessage::Status(self.status())),
            ClientMessage::History { limit, offset } => {
                if !matches!(self.target, SessionTarget::Agent { .. }) {
                    return Err(ProtocolError::new(
                        "unsupported_operation",
                        "Workflow has no agent history",
                    ));
                }
                if limit > 100 {
                    return Err(ProtocolError::new(
                        "invalid_message",
                        "History page limit is 100",
                    ));
                }
                let runtime = SessionRuntime::with_host_dir(&self.registry.home);
                let response = runtime
                    .query_page(
                        &self.key.target,
                        &self.key.user,
                        &self.key.session,
                        Some(limit),
                        Some(offset),
                    )
                    .await
                    .map_err(|e| ProtocolError::new("internal_error", e))?;
                let fae_agent::SessionResponse::History { messages, .. } = response else {
                    unreachable!()
                };
                Ok(ServerMessage::History { messages })
            }
            ClientMessage::Clean => {
                if !matches!(self.target, SessionTarget::Agent { .. }) {
                    return Err(ProtocolError::new(
                        "unsupported_operation",
                        "Workflow cannot clean history",
                    ));
                }
                self.registry.begin_clean(&self.key)?;
                let result = async {
                    clean_session_data(
                        &SessionRuntime::with_host_dir(&self.registry.home),
                        &self.key.target,
                        &self.key.user,
                        &self.key.session,
                    )
                    .await?;
                    if let Some(engine) = &self.engine {
                        engine.exit().await?;
                    }
                    self.engine = None;
                    self.registry.remove(&self.key, &self.stream);
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                self.registry.end_clean(&self.key);
                result.map_err(|e| {
                    ProtocolError::new(
                        if e.to_string().contains("busy") {
                            "cleanup_busy"
                        } else {
                            "internal_error"
                        },
                        e,
                    )
                })?;
                self.stop = true;
                Ok(ServerMessage::Cleaned)
            }
            ClientMessage::Close { cancel_active } => {
                if cancel_active && self.round.is_some() {
                    self.cancel().await?;
                }
                Ok(ServerMessage::Closed)
            }
            ClientMessage::Open(_) => Err(ProtocolError::new(
                "invalid_message",
                "Connection already bound",
            )),
        }
    }
    pub async fn run(mut self, mut rx: mpsc::Receiver<Command>) {
        while let Some(command) = rx.recv().await {
            match command {
                Command::Attach {
                    out,
                    request,
                    resume,
                } => {
                    self.lease = out.id.clone();
                    self.out = Some(out);
                    self.info.status = self.status();
                    self.info.resumed = resume.is_some();
                    let replay = resume
                        .as_ref()
                        .map(|r| self.replay.after(r.after_seq))
                        .transpose();
                    self.send(
                        self.envelope(Some(&request), ServerMessage::Opened(self.info.clone())),
                    );
                    match replay {
                        Ok(Some(events)) => {
                            for event in events {
                                self.send(event);
                            }
                        }
                        Err(mut error) => {
                            if let Some(detail) = error
                                .detail
                                .as_mut()
                                .and_then(serde_json::Value::as_object_mut)
                            {
                                detail.insert(
                                    "status".into(),
                                    serde_json::to_value(self.status()).unwrap(),
                                );
                            }
                            self.send(self.envelope(None, ServerMessage::Error(error)));
                        }
                        _ => {}
                    }
                    if matches!(self.target, SessionTarget::Workflow { .. })
                        && !self.workflow_launched
                        && self.round.is_none()
                        && resume.is_none()
                    {
                        self.workflow_launched = true;
                        match self.start(None).await {
                            Ok(round) => self.send(self.envelope(
                                None,
                                ServerMessage::Ack {
                                    round_id: Some(round),
                                },
                            )),
                            Err(error) => {
                                self.send(self.envelope(None, ServerMessage::Error(error)))
                            }
                        }
                    }
                }
                Command::Request(request) => {
                    let content = serde_json::to_string(&request.body).unwrap();
                    match self.requests.get(&request.request_id, &content) {
                        Ok(Some(response)) => self.send(response),
                        Err(error) => self.send(
                            self.envelope(Some(&request.request_id), ServerMessage::Error(error)),
                        ),
                        Ok(None) => {
                            let closed = matches!(request.body, ClientMessage::Close { .. });
                            let body = self
                                .request(request.body)
                                .await
                                .unwrap_or_else(ServerMessage::Error);
                            let response = self.envelope(Some(&request.request_id), body);
                            self.requests
                                .push(request.request_id, content, response.clone());
                            self.send(response);
                            if closed {
                                self.out = None;
                                self.registry.detach(&self.key, &self.stream, &self.lease);
                            }
                        }
                    }
                }
                Command::Started { session, context } => {
                    self.session = Some(session);
                    self.context = context;
                    if self.cancel_pending {
                        let _ = self.cancel().await;
                    }
                }
                Command::Output(output) => {
                    if let Some(round) = self.round.clone() {
                        let seq = self.replay.next_seq();
                        let event = self.envelope(
                            None,
                            ServerMessage::Event {
                                seq,
                                round_id: round,
                                output,
                            },
                        );
                        self.replay.push(event.clone());
                        self.send(event);
                    }
                }
                Command::Finished(outcome) => {
                    if let Some(round) = self.round.take() {
                        self.registry.finish_round(&self.key);
                        self.cancel_pending = false;
                        self.context = None;
                        let seq = self.replay.next_seq();
                        let finished = self.envelope(
                            None,
                            ServerMessage::Finished {
                                seq,
                                round_id: round,
                                outcome,
                            },
                        );
                        self.replay.push(finished.clone());
                        self.send(finished);
                    }
                }
                Command::Detach { lease } => {
                    if lease == self.lease {
                        self.out = None;
                        self.registry.detach(&self.key, &self.stream, &lease);
                    }
                }
                Command::Stop(reply) => {
                    self.stop = true;
                    self.stop_reply = reply;
                    if self.round.is_some() {
                        let _ = self.cancel().await;
                    }
                }
            }
            if self.stop && self.round.is_none() {
                break;
            }
        }
        let result = if let Some(engine) = self.engine {
            engine.exit().await.map_err(anyhow::Error::from)
        } else {
            Ok(())
        };
        if let Some(reply) = self.stop_reply {
            let _ = reply.send(result);
        }
        self.registry.remove(&self.key, &self.stream);
        // Drop the sender after the final reply; transport drains queued frames.
    }
}

async fn validate_input(input: &fae_agent::SessionInputData) -> Result<(), ProtocolError> {
    if input.text.trim().is_empty() && input.images.as_ref().is_none_or(|v| v.is_empty()) {
        return Err(ProtocolError::new(
            "invalid_message",
            "Input and images cannot both be empty",
        ));
    }
    for path in input
        .images
        .iter()
        .flatten()
        .chain(input.files.iter().flatten())
    {
        if path.starts_with("http://") || path.starts_with("https://") || path.starts_with("data:")
        {
            continue;
        }
        if !std::path::Path::new(path).is_absolute() {
            return Err(ProtocolError::new(
                "invalid_message",
                "Attachment path must be absolute",
            ));
        }
        tokio::fs::File::open(path).await.map_err(|e| {
            ProtocolError::new("invalid_message", format!("Cannot read {path}: {e}"))
        })?;
    }
    Ok(())
}

pub async fn clean_session_data(
    runtime: &SessionRuntime,
    agent: &str,
    user: &str,
    session: &str,
) -> anyhow::Result<()> {
    let todos = TodoTool::with_host_dir(runtime.host_dir());
    let guard = todos.lock_for_cleanup(agent, user).await?;
    runtime.delete(agent, user, session).await?;
    guard.delete_state().await?;
    Ok(())
}
