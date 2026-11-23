use super::{
    connection::Outbound,
    engine::build_engine,
    registry::{Key, Registry},
    replay::Replay,
};
use crate::{
    protocol::ProtocolError,
    session_host::{DefinitionTarget, LarkPrincipal, SessionFacts, StoredOutcome},
};
use fae_agent::{
    ContextData, ExecutionId, ExecutionOutcome, ExecutionSettings, ExecutionStatus, MemoryView,
    PlanTarget, RequestId, SessionConfig, SessionControlError, SessionControlEvent, SessionHandle,
    SessionId, SessionLifecycle, SessionOperation, SessionReservation, SingleAgentEnv,
    SingleAgentPlanBuilder, UserId, UserInfo, WorkflowEnv, WorkspaceBinding, protocol::*,
};
use fae_engine::{Engine, SessionCoordinator, SessionRuntime, TodoTool};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, oneshot};

pub enum Command {
    Create {
        request: Message,
        out: Outbound,
        reply: oneshot::Sender<ResponseMessage>,
    },
    Request {
        request: Message,
        out: Outbound,
    },
    Detach {
        lease: String,
    },
    Control(SessionControlEvent),
    InternalStart {
        input: String,
        occurrence: fae_agent::TimerOccurrence,
        reply: oneshot::Sender<Result<String, ProtocolError>>,
    },
    Stop(Option<oneshot::Sender<anyhow::Result<()>>>),
}

pub struct Actor {
    registry: Arc<Registry>,
    key: Key,
    stream: String,
    target: DefinitionTarget,
    info: SessionFacts,
    initial_config: Option<SessionConfig>,
    handle: Option<SessionHandle>,
    builder: SingleAgentPlanBuilder,
    lark_hook: Option<crate::lark::hook::LarkHook>,
    engine: Option<Engine>,
    subscribers: HashMap<String, Outbound>,
    external_request_id: Option<String>,
    stop: bool,
    stop_reply: Option<oneshot::Sender<anyhow::Result<()>>>,
    replay: Replay,
    timer_occurrence: Option<fae_agent::TimerOccurrence>,
    round_reserved: bool,
    done: tokio::sync::watch::Sender<Option<String>>,
}

impl Actor {
    pub fn new(
        registry: Arc<Registry>,
        key: Key,
        stream: String,
        target: DefinitionTarget,
        info: SessionFacts,
        builder: SingleAgentPlanBuilder,
        _tx: mpsc::Sender<Command>,
        done: tokio::sync::watch::Sender<Option<String>>,
    ) -> Self {
        let now = fae_agent::Timestamp::now();
        let config = SessionConfig {
            session_id: SessionId::new(info.session_id.clone()).expect("validated session"),
            name: info.name.clone(),
            lifecycle: SessionLifecycle::Open,
            created_at: now.clone(),
            updated_at: now,
            target: match &target {
                DefinitionTarget::Agent { .. } => PlanTarget::Agent {
                    agent_id: key.target.clone(),
                },
                DefinitionTarget::Workflow { workflow_id, .. } => PlanTarget::Workflow {
                    workflow_id: workflow_id.clone(),
                },
            },
            workspace: WorkspaceBinding {
                root: info.workspace.to_string_lossy().into(),
                project_files: vec![],
            },
            options: Default::default(),
            channels: vec![],
        };
        Self {
            registry,
            key,
            stream,
            target,
            info,
            initial_config: Some(config),
            handle: None,
            builder,
            lark_hook: None,
            engine: None,
            subscribers: HashMap::new(),
            external_request_id: None,
            stop: false,
            stop_reply: None,
            replay: Replay::new(),
            timer_occurrence: None,
            round_reserved: false,
            done,
        }
    }
    pub fn configure(&mut self, create: &SessionCreateRequest) {
        let config = self.initial_config.as_mut().expect("not running");
        config.name = create.name.clone();
        config.target = create.target.clone();
        config.options = create.options.clone();
        config.channels = create.channels.clone();
    }
    pub fn reserve_internal_round(&mut self) {
        self.round_reserved = true;
    }
    pub fn bind_lark_environment(&mut self, binding: LarkPrincipal) {
        let hook = crate::lark::hook::LarkHook::new(
            self.info.target_id.clone(),
            self.info.session_id.clone(),
            binding,
        );
        self.builder.add_hook(hook.clone());
        self.lark_hook = Some(hook);
    }
    fn handle(&self) -> &SessionHandle {
        self.handle.as_ref().expect("actor initialized")
    }
    fn summary(&self) -> SessionSummary {
        self.handle().read_summary()
    }
    fn active(&self) -> Option<ExecutionId> {
        self.summary().active_execution
    }
    fn storage_key(&self) -> Key {
        let config = self.summary().session_config;
        let (kind, target) = match config.target {
            PlanTarget::Agent { agent_id } => ("agent", agent_id),
            PlanTarget::Workflow { workflow_id } => ("workflow", workflow_id),
        };
        Key {
            kind: kind.into(),
            target,
            user: self.key.user.clone(),
            session: self.key.session.clone(),
        }
    }
    fn event(&mut self, payload: EventPayload, request_id: Option<RequestId>) {
        let seq = self.replay.next_seq();
        let event = Message::event(
            self.handle().key().session_id,
            self.stream.clone(),
            seq,
            request_id,
            payload,
        )
        .unwrap();
        self.replay.push(event.clone());
        let failed: Vec<_> = self
            .subscribers
            .iter()
            .filter(|(_, out)| !out.send(ServerFrame::Event(event.clone())))
            .map(|(id, _)| id.clone())
            .collect();
        for id in failed {
            if let Some(out) = self.subscribers.remove(&id) {
                if !self.subscribers.values().any(|o| o.id == out.id) {
                    self.registry.detach(&self.key, &self.stream, &out.id);
                }
            }
        }
    }
    async fn cancel(&mut self, reason: &str) -> Result<(), ProtocolError> {
        let id = self
            .active()
            .ok_or_else(|| ProtocolError::new("round_not_active", "Execution is not active"))?;
        self.handle()
            .stop(&id, reason.into())
            .map_err(control_error)?;
        Ok(())
    }
    async fn start(
        &mut self,
        settings: ExecutionSettings,
        content: Content,
        request_id: Option<RequestId>,
    ) -> Result<String, ProtocolError> {
        // Validate input before creating the Preparing execution.
        let input = match &settings.target {
            PlanTarget::Agent { .. } => Some(input_data(&content).await?),
            PlanTarget::Workflow { .. } => {
                if settings.options.mode.is_some() {
                    return Err(ProtocolError::new(
                        "invalid_message",
                        "Workflow does not accept Agent mode",
                    ));
                }
                workflow_input(&content)?;
                None
            }
        };
        if self.round_reserved {
            self.round_reserved = false;
        } else {
            let mut binding = self.key.clone();
            binding.target = match &settings.target {
                PlanTarget::Agent { agent_id } => agent_id.clone(),
                PlanTarget::Workflow { workflow_id } => workflow_id.clone(),
            };
            self.registry.reserve_round(&binding)?;
        }
        let timer = match &settings.target {
            PlanTarget::Agent { agent_id } => Some(Arc::new(fae_agent::TimerExecutionBinding {
                owner: fae_agent::TimerOwner {
                    agent_id: agent_id.clone(),
                    user_id: self.key.user.clone(),
                },
                source: match &self.target {
                    DefinitionTarget::Agent { source } if agent_id == &self.key.target => {
                        source.clone()
                    }
                    _ => fae_agent::SingleAgentSource::AgentId(agent_id.clone()),
                },
                workspace: settings.workspace.clone().into(),
            })),
            _ => None,
        };
        let environment = fae_agent::ExecutionEnvironment {
            origin: self.timer_occurrence.as_ref().map(|_| "timer".into()),
            timer,
        };
        let execution = match self
            .handle()
            .prepare_with_environment(settings.clone(), environment)
        {
            Ok(execution) => execution,
            Err(error) => {
                self.registry.finish_round(&self.key);
                return Err(control_error(error));
            }
        };
        if let Some(request_id) = request_id {
            execution
                .session
                .report(
                    &execution.execution_id,
                    fae_agent::ExecutionReport::InitialInputAccepted(request_id),
                )
                .map_err(control_error)?;
        }
        let id = execution.execution_id.to_string();
        let engine = self.engine.as_ref().unwrap().clone();
        let user = self.key.user.clone();
        let session_id = self.key.session.clone();
        let original_target = self.target.clone();
        let original_id = self.key.target.clone();
        tokio::spawn(async move {
            match settings.target {
                PlanTarget::Agent { agent_id } => {
                    let source = match original_target {
                        DefinitionTarget::Agent { source } if agent_id == original_id => source,
                        _ => fae_agent::SingleAgentSource::AgentId(agent_id),
                    };
                    let input = input.unwrap();
                    let (env, _) = SingleAgentEnv::new_with_user_id(source, input.text, user);
                    let mut env = env
                        .with_images(input.images.unwrap_or_default())
                        .with_session_id(session_id);
                    env.mode = settings.options.mode.map(|_| "todo".into());
                    if let Err(error) = engine.execute_session(execution, env).await {
                        eprintln!("session execution: {error:#}");
                    }
                }
                PlanTarget::Workflow { workflow_id } => {
                    let (env, _) = WorkflowEnv::new_with_user_id(
                        workflow_id,
                        workflow_input(&content).unwrap(),
                        user,
                    );
                    if let Err(error) = engine.execute_session(execution, env).await {
                        eprintln!("workflow execution: {error:#}");
                    }
                }
            }
        });
        Ok(id)
    }
    async fn input_receipt(
        &mut self,
        request: &Message,
        content: &Content,
        steer: bool,
    ) -> Result<Option<ResponsePayload>, ProtocolError> {
        // Durable gateway receipts use the public request_id, never an extra payload field.
        if self.lark_hook.is_none() {
            return Ok(None);
        }
        let id = request.request_id.as_ref().unwrap().as_str();
        let digest = crate::lark::identity::hash(&[&serde_json::to_string(request).unwrap()]);
        let _lock = super::acceptance::lock(&self.registry.home, id).await?;
        if let Some(receipt) =
            super::acceptance::read(&self.registry.home, id, &self.key, Some(&digest))?
        {
            if receipt.uncertain {
                return Err(ProtocolError::new(
                    "acceptance_unknown",
                    "Acceptance is unknown; do not retry",
                ));
            }
            let execution_id =
                ExecutionId::new(receipt.round_id.ok_or_else(|| {
                    ProtocolError::new("acceptance_unknown", "Missing execution")
                })?)
                .unwrap();
            return Ok(Some(if receipt.supplemented {
                ResponsePayload::SessionExecutionSteer(InputAccepted {
                    execution_id,
                    request_id: request.request_id.clone().unwrap(),
                    accepted: true,
                })
            } else {
                ResponsePayload::SessionExecutionStart(ExecutionAccepted {
                    execution_id,
                    accepted: true,
                })
            }));
        }
        let environment = self
            .lark_hook
            .as_ref()
            .unwrap()
            .load_message(&self.registry.home, id)
            .await
            .map_err(|e| ProtocolError::new("lark_environment", e))?;
        // Validate resource references before storing uncertain admission.
        let _ = input_data(content).await?;
        super::acceptance::write(
            &self.registry.home,
            id,
            &self.key,
            digest,
            None,
            true,
            steer,
        )?;
        self.lark_hook
            .as_ref()
            .unwrap()
            .replace_environment(environment);
        Ok(None)
    }
    async fn request(
        &mut self,
        request: &Message,
        out: &Outbound,
    ) -> Result<ResponsePayload, ProtocolError> {
        let body = request
            .as_request()
            .map_err(|e| ProtocolError::new("invalid_message", e))?;
        match body {
            RequestPayload::SessionCreate(_) => Ok(ResponsePayload::SessionCreate(self.summary())),
            RequestPayload::SessionGet(_) => Ok(ResponsePayload::SessionGet(self.summary())),
            RequestPayload::SessionSubscribe(cursor) => {
                if cursor.stream_id.as_ref().is_some_and(|s| s != &self.stream) {
                    return Err(ProtocolError::new(
                        "resume_expired",
                        "Stream no longer exists; execution was not rerun",
                    ));
                }
                let replay = cursor
                    .after_seq
                    .map(|seq| self.replay.after(seq))
                    .transpose()?;
                let subscription_id = self
                    .subscribers
                    .iter()
                    .find(|(_, o)| o.id == out.id)
                    .map(|(id, _)| id.clone())
                    .unwrap_or_else(wd_tools::uuid::v4);
                self.subscribers
                    .insert(subscription_id.clone(), out.clone());
                self.registry.subscribed(&self.key, &out.id);
                // Response is queued before replay and live events.
                let data = ResponsePayload::SessionSubscribe(SubscribeResult {
                    subscription_id,
                    stream_id: self.stream.clone(),
                    snapshot_seq: self.replay.seq,
                    snapshot: self.summary(),
                    replay: ReplayCapabilities {
                        max_events: 4096,
                        max_bytes: 16 * 1024 * 1024,
                        persistent: false,
                    },
                });
                let response = ResponseMessage::success(
                    request,
                    request.session_id.clone(),
                    "Subscribed",
                    Some(data.clone()),
                )
                .unwrap();
                out.send(ServerFrame::Response(response));
                if let Some(events) = replay {
                    for event in events {
                        out.send(ServerFrame::Event(event));
                    }
                }
                Ok(data)
            }
            RequestPayload::SessionUnsubscribe(args) => {
                if self
                    .subscribers
                    .get(&args.subscription_id)
                    .is_some_and(|o| o.id != out.id)
                {
                    return Err(ProtocolError::new(
                        "forbidden",
                        "Subscription belongs to another connection",
                    ));
                }
                let unsubscribed = self.subscribers.remove(&args.subscription_id).is_some();
                if !self.subscribers.values().any(|o| o.id == out.id) {
                    self.registry.detach(&self.key, &self.stream, &out.id);
                }
                Ok(ResponsePayload::SessionUnsubscribe(UnsubscribeResult {
                    subscription_id: args.subscription_id.clone(),
                    unsubscribed,
                }))
            }
            RequestPayload::SessionExecutionStart(args) => {
                let config = self.summary().session_config;
                let target = args.target.clone().unwrap_or(config.target);
                if self.lark_hook.is_some() && target != self.summary().session_config.target {
                    return Err(ProtocolError::new(
                        "session_binding_conflict",
                        "Lark session target is immutable",
                    ));
                }
                let key = self.storage_key();
                let workspace = super::workspace::SessionWorkspace::new(&self.registry.home, &key)?
                    .resolve(
                        args.workspace.as_deref().map(std::path::Path::new),
                        Some(std::path::Path::new(&config.workspace.root)),
                    )?;
                let settings = ExecutionSettings {
                    target,
                    workspace: workspace.to_string_lossy().into(),
                    options: args.options.clone().unwrap_or(config.options),
                    definitions: vec![],
                };
                if let Some(response) = self.input_receipt(request, &args.input, false).await? {
                    return Ok(response);
                }
                let id = self
                    .start(settings, args.input.clone(), request.request_id.clone())
                    .await?;
                let execution_id = ExecutionId::new(id.clone()).unwrap();
                if self.lark_hook.is_some() {
                    let rid = request.request_id.as_ref().unwrap().as_str();
                    let digest =
                        crate::lark::identity::hash(&[&serde_json::to_string(request).unwrap()]);
                    super::acceptance::write(
                        &self.registry.home,
                        rid,
                        &self.key,
                        digest,
                        Some(id),
                        false,
                        false,
                    )?;
                    self.external_request_id = Some(rid.into());
                }
                Ok(ResponsePayload::SessionExecutionStart(ExecutionAccepted {
                    execution_id,
                    accepted: true,
                }))
            }
            RequestPayload::SessionExecutionSteer(args) => {
                input_data(&args.input).await?;
                if let Some(response) = self.input_receipt(request, &args.input, true).await? {
                    return Ok(response);
                }
                self.handle()
                    .steer(
                        &args.execution_id,
                        request.request_id.clone().unwrap(),
                        args.input.clone(),
                    )
                    .map_err(control_error)?;
                if self.lark_hook.is_some() {
                    let rid = request.request_id.as_ref().unwrap().as_str();
                    let digest =
                        crate::lark::identity::hash(&[&serde_json::to_string(request).unwrap()]);
                    super::acceptance::write(
                        &self.registry.home,
                        rid,
                        &self.key,
                        digest,
                        Some(args.execution_id.to_string()),
                        false,
                        true,
                    )?;
                }
                Ok(ResponsePayload::SessionExecutionSteer(InputAccepted {
                    execution_id: args.execution_id.clone(),
                    request_id: request.request_id.clone().unwrap(),
                    accepted: true,
                }))
            }
            RequestPayload::SessionExecutionStop(args) => {
                let accepted = self
                    .handle()
                    .stop(
                        &args.execution_id,
                        args.reason
                            .clone()
                            .unwrap_or_else(|| "Cancelled by client".into()),
                    )
                    .map_err(control_error)?;
                let status = self
                    .handle()
                    .execution(Some(&args.execution_id))
                    .map_err(control_error)?
                    .status;
                Ok(ResponsePayload::SessionExecutionStop(ExecutionStopResult {
                    execution_id: args.execution_id.clone(),
                    accepted,
                    status,
                }))
            }
            RequestPayload::SessionExecutionGet(args) => {
                let mut execution = match self.handle().execution(args.execution_id.as_ref()) {
                    Ok(view) => Some(view),
                    Err(error) if args.execution_id.is_some() => return Err(control_error(error)),
                    Err(_) => None,
                };
                let acceptance = if let Some(rid) = &args.input_request_id {
                    let latest = self.handle().execution(None).ok();
                    let input = latest
                        .as_ref()
                        .and_then(|e| e.inputs.iter().find(|i| &i.request_id == rid))
                        .cloned();
                    let receipt = if self.lark_hook.is_some() {
                        super::acceptance::read(&self.registry.home, rid.as_str(), &self.key, None)?
                    } else {
                        None
                    };
                    let execution_id =
                        input.as_ref().map(|i| i.execution_id.clone()).or_else(|| {
                            receipt
                                .as_ref()
                                .and_then(|r| r.round_id.as_ref())
                                .and_then(|id| ExecutionId::new(id.clone()).ok())
                        });
                    let certainty =
                        if input.is_some() || receipt.as_ref().is_some_and(|r| !r.uncertain) {
                            AcceptanceCertainty::Known
                        } else {
                            AcceptanceCertainty::Unknown
                        };
                    if let Some(id) = &execution_id {
                        execution = execution.filter(|e| &e.execution_id == id);
                    }
                    let outcome = receipt
                        .as_ref()
                        .and_then(|r| r.outcome.as_ref())
                        .map(StoredOutcome::public)
                        .or_else(|| execution.as_ref().and_then(|e| e.outcome.clone()));
                    Some(AcceptanceFact {
                        request_id: rid.clone(),
                        certainty,
                        execution_id,
                        input,
                        outcome,
                    })
                } else {
                    None
                };
                Ok(ResponsePayload::SessionExecutionGet(ExecutionGetResult {
                    execution,
                    acceptance,
                }))
            }
            RequestPayload::SessionConversationList(args) => {
                if args.limit > 100 {
                    return Err(ProtocolError::new(
                        "invalid_message",
                        "History page limit is 100",
                    ));
                }
                let key = self.storage_key();
                let response = SessionRuntime::with_host_dir(&self.registry.home)
                    .query_page(
                        &key.target,
                        &key.user,
                        &key.session,
                        Some(args.limit),
                        Some(args.offset),
                    )
                    .await
                    .map_err(|e| ProtocolError::new("internal_error", e))?;
                let fae_agent::SessionResponse::History { messages, .. } = response else {
                    unreachable!()
                };
                let next_offset = (messages.len() == args.limit && args.limit > 0)
                    .then_some(args.offset + messages.len());
                Ok(ResponsePayload::SessionConversationList(ConversationPage {
                    partition: ConversationPartition {
                        target_id: key.target,
                        user_id: key.user,
                        session_id: self.handle().key().session_id,
                    },
                    offset: args.offset,
                    next_offset,
                    messages,
                }))
            }
            RequestPayload::SessionConversationClear(_) => {
                let reservation =
                    SessionReservation::new(self.handle().clone(), SessionOperation::Clear)
                        .map_err(control_error)?;
                let key = self.storage_key();
                self.registry.begin_clean(&key)?;
                let result = SessionRuntime::with_host_dir(&self.registry.home)
                    .delete(&key.target, &key.user, &key.session)
                    .await;
                self.registry.end_clean(&key);
                result.map_err(|e| ProtocolError::new("internal_error", e))?;
                self.handle()
                    .commit_clear(reservation.token())
                    .map_err(control_error)?;
                Ok(ResponsePayload::SessionConversationClear(
                    ConversationClearResult { cleared: true },
                ))
            }
            RequestPayload::SessionConfigUpdate(args) => {
                let reservation =
                    SessionReservation::new(self.handle().clone(), SessionOperation::Configure)
                        .map_err(control_error)?;
                let mut update = args.clone();
                if let Some(target) = &update.target {
                    if self.lark_hook.is_some() && target != &self.summary().session_config.target {
                        return Err(ProtocolError::new(
                            "session_binding_conflict",
                            "Lark session target is immutable",
                        ));
                    }
                    self.validate_target(target).await?;
                }
                if let Some(workspace) = &update.workspace {
                    let store = super::workspace::SessionWorkspace::new(
                        &self.registry.home,
                        &self.storage_key(),
                    )?;
                    let path = store.resolve(Some(std::path::Path::new(workspace)), None)?;
                    store
                        .save(&path)
                        .map_err(|e| ProtocolError::new("storage_failed", e))?;
                    update.workspace = Some(path.to_string_lossy().into());
                }
                let config = self
                    .handle()
                    .commit_config(reservation.token(), update)
                    .map_err(control_error)?;
                Ok(ResponsePayload::SessionConfigUpdate(config))
            }
            RequestPayload::SessionArchive(_) => {
                self.handle().archive().map_err(control_error)?;
                Ok(ResponsePayload::SessionArchive(self.summary()))
            }
            _ => Err(ProtocolError::new(
                "unsupported_operation",
                "Expected session operation",
            )),
        }
    }
    async fn validate_target(&self, target: &PlanTarget) -> Result<(), ProtocolError> {
        match target {
            PlanTarget::Agent { agent_id } => {
                self.builder
                    .load_config(&fae_agent::SingleAgentSource::AgentId(agent_id.clone()))
                    .await
                    .map_err(|e| ProtocolError::new("config_not_found", e))?;
            }
            PlanTarget::Workflow { workflow_id } => {
                use fae_agent::WorkflowMetadataLoader;
                fae_agent::FAEWorkflowMetadataLoader::with_home_dir(&self.registry.home)
                    .load(workflow_id)
                    .await
                    .map_err(|e| ProtocolError::new("config_not_found", e))?;
            }
        }
        Ok(())
    }
    async fn control_event(&mut self, event: SessionControlEvent) {
        let payload = match event {
            SessionControlEvent::ConfigUpdated(config) => {
                EventPayload::ConfigUpdated(ConfigUpdated { config })
            }
            SessionControlEvent::ConversationAppended(data) => {
                EventPayload::ConversationAppended(data)
            }
            SessionControlEvent::Output {
                execution_id,
                invocation_id,
                output_id,
                source,
                content,
            } => EventPayload::OutputDelta(OutputDelta {
                execution_id,
                invocation_id,
                output_id,
                source,
                content,
            }),
            SessionControlEvent::InvocationChanged {
                execution_id,
                invocation,
            } => EventPayload::InvocationChanged(InvocationChanged {
                execution_id,
                invocation,
            }),
            SessionControlEvent::InputProcessed(input) => match input.status {
                fae_agent::InputStatus::Applied => EventPayload::InputApplied(InputApplied {
                    execution_id: input.execution_id,
                    request_id: input.request_id,
                    applied_at: input.processed_at.unwrap_or_else(fae_agent::Timestamp::now),
                }),
                _ => EventPayload::InputNotApplied(InputNotApplied {
                    execution_id: input.execution_id,
                    request_id: input.request_id,
                    reason: input
                        .reason
                        .unwrap_or_else(|| "Input was not applied".into()),
                    processed_at: input.processed_at.unwrap_or_else(fae_agent::Timestamp::now),
                }),
            },
            SessionControlEvent::StateChanged(view) => {
                if view.status == ExecutionStatus::Running {
                    if let (Some(timer), Some(o)) = (&self.registry.timer, &self.timer_occurrence) {
                        if let Err(e) = timer
                            .acknowledge(
                                &o.owner,
                                &o.occurrence_id,
                                fae_agent::TimerOccurrenceStatus::Running,
                                Some(view.execution_id.to_string()),
                                None,
                            )
                            .await
                        {
                            eprintln!("timer running receipt: {e:#}");
                            let _ = self.cancel("Timer receipt failed").await;
                        }
                    }
                }
                EventPayload::ExecutionStateChanged(ExecutionStateChanged {
                    execution_id: view.execution_id,
                    status: view.status,
                    reason: None,
                })
            }
            SessionControlEvent::Finished {
                execution_id,
                outcome,
            } => {
                let stored = match &outcome {
                    ExecutionOutcome::Succeeded { result } => StoredOutcome::Succeeded {
                        result: match result.as_slice() {
                            [ContentBlock::Json { value }] => value.clone(),
                            _ => serde_json::to_value(result).unwrap(),
                        },
                    },
                    ExecutionOutcome::Failed { error } => StoredOutcome::Failed {
                        message: error.message.clone(),
                    },
                    ExecutionOutcome::Cancelled { reason } => StoredOutcome::Cancelled {
                        reason: reason.clone(),
                    },
                };
                if let Some(o) = self.timer_occurrence.take() {
                    if let Some(timer) = &self.registry.timer {
                        use fae_agent::TimerOccurrenceStatus as Status;
                        let (status, error) = match &stored {
                            StoredOutcome::Succeeded { .. } => (Status::Succeeded, None),
                            StoredOutcome::Failed { message } => {
                                (Status::Failed, Some(message.clone()))
                            }
                            StoredOutcome::Cancelled { reason } => {
                                (Status::Cancelled, Some(reason.clone()))
                            }
                        };
                        if let Err(e) = timer
                            .acknowledge(
                                &o.owner,
                                &o.occurrence_id,
                                status,
                                Some(execution_id.to_string()),
                                error,
                            )
                            .await
                        {
                            eprintln!("timer terminal receipt: {e:#}");
                        }
                    }
                }
                if let Some(rid) = self.external_request_id.take() {
                    if let Err(e) =
                        super::acceptance::finish(&self.registry.home, &rid, &self.key, stored)
                    {
                        eprintln!("external terminal receipt: {e}");
                    }
                }
                self.stop |= self.registry.finish_round(&self.key);
                EventPayload::ExecutionFinished(ExecutionFinished {
                    execution_id,
                    outcome,
                })
            }
        };
        self.event(payload, None);
    }
    pub async fn run(mut self, mut rx: mpsc::Receiver<Command>) {
        let mut builder = self.builder.clone();
        builder.set_workspace(self.info.workspace.clone());
        builder.add_hook(super::workspace::WorkspaceHookBuilder::new(
            self.info.workspace.clone(),
        ));
        let engine = build_engine(
            fae_agent::FAEWorkflowMetadataLoader::with_home_dir(&self.registry.home),
            builder,
            self.registry.timer.clone(),
            self.registry.plugins.clone(),
        )
        .await;
        let coordinator = SessionCoordinator::new(
            engine.clone(),
            ContextData {
                user_info: UserInfo {
                    user_id: UserId::new(self.key.user.clone()).unwrap(),
                    name: None,
                },
                session_config: self.initial_config.take().unwrap(),
                memory_view: MemoryView::default(),
                latest_execution_view: None,
            },
        )
        .expect("validated session config");
        coordinator.set_data_source(Arc::new(fae_engine::SessionFileSource::new(
            &self.registry.home,
        )));
        let handle = coordinator.handle();
        let mut events = handle.subscribe();
        self.registry.bind_handle(&self.key, handle.clone());
        self.handle = Some(handle);
        self.engine = Some(engine);
        loop {
            // Drain coordinator facts before accepting operations that depend on idle state.
            let command = tokio::select! {
                biased;
                event = events.recv() => match event {
                    Some(event) => Command::Control(event),
                    None => break,
                },
                command = rx.recv() => match command {
                    Some(command) => command,
                    None => break,
                },
            };
            match command {
                Command::Create {
                    request,
                    out,
                    reply,
                } => {
                    let response = ResponseMessage::success(
                        &request,
                        Some(self.handle().key().session_id),
                        "OK",
                        Some(ResponsePayload::SessionCreate(self.summary())),
                    )
                    .unwrap();
                    out.send(ServerFrame::Response(response.clone()));
                    let _ = reply.send(response);
                }
                Command::Request { request, out } => {
                    let cache = self.registry.requests_for(&self.key.user);
                    let mut requests = cache.lock().await;
                    let content = serde_json::to_string(&request).unwrap();
                    let rid = request.request_id.as_ref().unwrap().to_string();
                    let result = match requests.get(&rid, &content) {
                        Ok(Some(response)) => {
                            out.send(ServerFrame::Response(response));
                            continue;
                        }
                        Err(error) => Err(error),
                        Ok(None) => self.request(&request, &out).await,
                    };
                    let response = match result {
                        Ok(data) => ResponseMessage::success(
                            &request,
                            Some(self.handle().key().session_id),
                            "OK",
                            Some(data),
                        )
                        .unwrap(),
                        Err(error) => ResponseMessage::error(
                            &request,
                            response_code(&error),
                            error.to_string(),
                        )
                        .unwrap(),
                    };
                    requests.push(rid, content, response.clone());
                    // Subscribe queues its own response before replay.
                    if request.path != MessagePath::Request(RequestPath::SessionSubscribe)
                        || response.payload.code != ResponseCode::Success
                    {
                        out.send(ServerFrame::Response(response));
                    }
                }
                Command::InternalStart {
                    input,
                    occurrence,
                    reply,
                } => {
                    self.timer_occurrence = Some(occurrence);
                    let config = self.summary().session_config;
                    let result = self
                        .start(
                            ExecutionSettings {
                                target: config.target,
                                workspace: config.workspace.root,
                                options: config.options,
                                definitions: vec![],
                            },
                            vec![ContentBlock::Text { text: input }],
                            None,
                        )
                        .await;
                    if result.is_err() {
                        self.stop = true;
                    }
                    let _ = reply.send(result);
                }
                Command::Control(event) => self.control_event(event).await,
                Command::Detach { lease } => {
                    self.subscribers.retain(|_, out| out.id != lease);
                    self.registry.detach(&self.key, &self.stream, &lease);
                }
                Command::Stop(reply) => {
                    self.stop = true;
                    self.stop_reply = reply;
                    if self.active().is_some() {
                        let _ = self.cancel("Service stopped").await;
                    }
                }
            }
            if self.stop && self.active().is_none() {
                break;
            }
        }
        let result = if let Some(engine) = self.engine {
            engine.exit().await.map_err(anyhow::Error::from)
        } else {
            Ok(())
        };
        let _ = self.done.send(Some(
            result
                .as_ref()
                .err()
                .map(|e| format!("{e:#}"))
                .unwrap_or_default(),
        ));
        if let Some(reply) = self.stop_reply {
            let _ = reply.send(result);
        }
        self.registry.remove(&self.key, &self.stream);
    }
}
pub fn response_code(error: &ProtocolError) -> ResponseCode {
    match error.code.as_str() {
        "session_busy"
        | "round_not_active"
        | "invalid_state"
        | "resume_gap"
        | "request_id_conflict"
        | "request_conflict"
        | "cleanup_busy"
        | "session_binding_conflict" => ResponseCode::Conflict,
        "resume_expired" => ResponseCode::NotFound,
        "resource_exhausted" => ResponseCode::Unavailable,
        "storage_failed" => ResponseCode::StorageError,
        "unsupported_operation" => ResponseCode::BadRequest,
        "forbidden" => ResponseCode::Forbidden,
        "session_not_found" | "config_not_found" => ResponseCode::NotFound,
        "invalid_message" | "invalid_workspace" | "invalid_session_id" => ResponseCode::BadRequest,
        _ => ResponseCode::InternalError,
    }
}
pub async fn input_data(content: &Content) -> Result<fae_agent::AgentInput, ProtocolError> {
    let mut text = String::new();
    let mut images = vec![];
    for block in content {
        match block {
            ContentBlock::Text { text: part } => text.push_str(part),
            ContentBlock::Resource { resource } => {
                let path = resource
                    .uri
                    .strip_prefix("file://")
                    .unwrap_or(&resource.uri);
                if !path.starts_with("https://")
                    && !path.starts_with("http://")
                    && !path.starts_with("data:")
                {
                    if !std::path::Path::new(path).is_absolute() {
                        return Err(ProtocolError::new(
                            "invalid_message",
                            "Resource path must be absolute",
                        ));
                    }
                    tokio::fs::File::open(path)
                        .await
                        .map_err(|e| ProtocolError::new("invalid_message", e))?;
                }
                if resource.media_type.starts_with("image/") {
                    images.push(path.to_string());
                } else {
                    return Err(ProtocolError::new(
                        "invalid_message",
                        "Agent input supports text and image resources",
                    ));
                }
            }
            ContentBlock::Json { .. } => {
                return Err(ProtocolError::new(
                    "invalid_message",
                    "Agent input requires text or resources",
                ));
            }
        }
    }
    if text.trim().is_empty() && images.is_empty() {
        return Err(ProtocolError::new(
            "invalid_message",
            "Input cannot be empty",
        ));
    }
    Ok(fae_agent::AgentInput::text(text).with_images(images))
}
fn workflow_input(content: &Content) -> Result<serde_json::Value, ProtocolError> {
    match content.as_slice() {
        [ContentBlock::Json { value }] => Ok(value.clone()),
        _ => Err(ProtocolError::new(
            "invalid_message",
            "Workflow input requires one JSON block",
        )),
    }
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

fn control_error(error: SessionControlError) -> ProtocolError {
    match error {
        SessionControlError::Conflict(message) => ProtocolError::new("session_busy", message),
        SessionControlError::NotFound(message) => ProtocolError::new("session_not_found", message),
        SessionControlError::Invalid(message) => ProtocolError::new("invalid_message", message),
    }
}
