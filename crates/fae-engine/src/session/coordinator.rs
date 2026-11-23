use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Mutex},
};

use fae_agent::{protocol::*, *};

use crate::Engine;

#[derive(Debug)]
struct ActiveExecution {
    ctx: Ctx,
    pending_cleanup: HashSet<InvocationId>,
    resources: HashSet<RequestId>,
    mailbox: VecDeque<QueuedInput>,
    candidate: Option<ExecutionOutcome>,
    cancel_reason: Option<String>,
    initial_input: Option<RequestId>,
}

#[derive(Debug)]
struct State {
    data: ContextData,
    active: Option<ActiveExecution>,
    reservation: Option<(String, SessionOperation)>,
    revision: u64,
    history_loaded: bool,
    user_memories_loaded: bool,
    project_files_loaded: bool,
}

/// Short synchronous commits only. No state lock is held across I/O or Plan work.
pub struct SessionCoordinator {
    engine: Engine,
    state: Mutex<State>,
    events: SessionEvents,
    weak: std::sync::Weak<Self>,
    source: Mutex<Option<Arc<dyn SessionDataSource>>>,
}

impl std::fmt::Debug for SessionCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCoordinator")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// Local host delivery must retain cleanup and terminal facts even during an output burst.
#[derive(Debug, Default)]
struct SessionEvents(Mutex<Vec<tokio::sync::mpsc::UnboundedSender<SessionControlEvent>>>);

impl SessionEvents {
    fn send(&self, event: SessionControlEvent) -> std::result::Result<(), ()> {
        self.0
            .lock()
            .unwrap()
            .retain(|sender| sender.send(event.clone()).is_ok());
        Ok(())
    }

    fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<SessionControlEvent> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        self.0.lock().unwrap().push(sender);
        receiver
    }
}

impl SessionCoordinator {
    pub fn new(engine: Engine, data: ContextData) -> ControlResult<Arc<Self>> {
        if data
            .latest_execution_view
            .as_ref()
            .is_some_and(|v| !v.status.is_terminal())
        {
            return Err(SessionControlError::Invalid(
                "a snapshot cannot restore an active execution".into(),
            ));
        }
        if !std::path::Path::new(&data.session_config.workspace.root).is_absolute() {
            return Err(SessionControlError::Invalid(
                "workspace must be absolute".into(),
            ));
        }
        Ok(Arc::new_cyclic(|weak| Self {
            engine,
            state: Mutex::new(State {
                data,
                active: None,
                reservation: None,
                revision: 0,
                history_loaded: false,
                user_memories_loaded: false,
                project_files_loaded: false,
            }),
            events: SessionEvents::default(),
            weak: weak.clone(),
            source: Mutex::new(None),
        }))
    }

    pub fn handle(self: &Arc<Self>) -> SessionHandle {
        SessionHandle(self.clone())
    }

    pub fn set_data_source(&self, source: Arc<dyn SessionDataSource>) {
        *self.source.lock().unwrap() = Some(source);
    }

    fn conflict(message: &str) -> SessionControlError {
        SessionControlError::Conflict(message.into())
    }

    fn idle(state: &State) -> ControlResult<()> {
        if state.active.is_some() || state.reservation.is_some() {
            return Err(Self::conflict("session is busy"));
        }
        if state.data.session_config.lifecycle != SessionLifecycle::Open {
            return Err(Self::conflict("session is archived"));
        }
        Ok(())
    }

    fn reserved(state: &State, token: &str, operation: SessionOperation) -> ControlResult<()> {
        if state
            .reservation
            .as_ref()
            .is_some_and(|(id, op)| id == token && *op == operation)
        {
            Ok(())
        } else {
            Err(Self::conflict("host operation reservation does not match"))
        }
    }

    fn default_partition(state: &State, partition: &ConversationPartition) -> bool {
        let target = match &state.data.session_config.target {
            PlanTarget::Agent { agent_id } => agent_id,
            PlanTarget::Workflow { workflow_id } => workflow_id,
        };
        partition.target_id == *target
            && partition.user_id == state.data.user_info.user_id.as_str()
            && partition.session_id == state.data.session_config.session_id
    }

    fn view<'a>(state: &'a mut State, id: &ExecutionId) -> ControlResult<&'a mut ExecutionView> {
        state
            .data
            .latest_execution_view
            .as_mut()
            .filter(|v| &v.execution_id == id)
            .ok_or_else(|| SessionControlError::NotFound("execution is no longer retained".into()))
    }

    fn publish(&self, state: &State) {
        if let Some(view) = &state.data.latest_execution_view {
            let _ = self
                .events
                .send(SessionControlEvent::StateChanged(view.clone()));
        }
    }

    fn commit_if_clean(&self, state: &mut State) {
        let Some(active) = &state.active else { return };
        if !active.pending_cleanup.is_empty()
            || !active.resources.is_empty()
            || active.candidate.is_none()
        {
            return;
        }
        let mut active = state.active.take().unwrap();
        let outcome = if let Some(reason) = active.cancel_reason {
            ExecutionOutcome::Cancelled { reason }
        } else {
            active.candidate.take().unwrap()
        };
        let view = state.data.latest_execution_view.as_mut().unwrap();
        let now = Timestamp::now();
        for record in &mut view.inputs {
            if record.status == InputStatus::Queued {
                record.status = InputStatus::NotApplied;
                record.reason = Some("execution closed before input was applied".into());
                record.processed_at = Some(now.clone());
                let _ = self
                    .events
                    .send(SessionControlEvent::InputProcessed(record.clone()));
            }
        }
        view.status = match &outcome {
            ExecutionOutcome::Succeeded { .. } => ExecutionStatus::Succeeded,
            ExecutionOutcome::Failed { .. } => ExecutionStatus::Failed,
            ExecutionOutcome::Cancelled { .. } => ExecutionStatus::Cancelled,
        };
        view.ended_at = Some(now.clone());
        view.outcome = Some(outcome.clone());
        state.data.session_config.updated_at = now;
        self.publish(state);
        let _ = self.events.send(SessionControlEvent::Finished {
            execution_id: state
                .data
                .latest_execution_view
                .as_ref()
                .unwrap()
                .execution_id
                .clone(),
            outcome,
        });
    }
}

#[async_trait::async_trait]
impl SessionControl for SessionCoordinator {
    fn key(&self) -> SessionKey {
        let state = self.state.lock().unwrap();
        SessionKey {
            user_id: state.data.user_info.user_id.clone(),
            session_id: state.data.session_config.session_id.clone(),
        }
    }

    fn read_context(&self) -> ContextData {
        self.state.lock().unwrap().data.clone()
    }

    async fn export_stored_context(&self) -> ControlResult<ContextData> {
        let source = self.source.lock().unwrap().clone().ok_or_else(|| {
            SessionControlError::Invalid("session storage source is not configured".into())
        })?;
        self.export_context(source.as_ref()).await
    }

    async fn export_context(&self, source: &dyn SessionDataSource) -> ControlResult<ContextData> {
        loop {
            let (data, revision) = {
                let state = self.state.lock().unwrap();
                if state.reservation.is_some() {
                    return Err(Self::conflict("session has a pending host operation"));
                }
                (state.data.clone(), state.revision)
            };
            let stored = source.read(&data).await?;
            let mut state = self.state.lock().unwrap();
            if state.revision != revision || state.reservation.is_some() {
                continue;
            }
            state.data.memory_view.history = stored.history;
            state.data.memory_view.history_offset = 0;
            state.data.memory_view.user_memories = stored.user_memories;
            state.data.session_config.workspace.project_files = stored.project_files;
            state.history_loaded = true;
            state.user_memories_loaded = true;
            state.project_files_loaded = true;
            state.revision += 1;
            return Ok(state.data.clone());
        }
    }

    fn reserve(&self, operation: SessionOperation) -> ControlResult<String> {
        let mut state = self.state.lock().unwrap();
        if operation == SessionOperation::Clear {
            if state.active.is_some() || state.reservation.is_some() {
                return Err(Self::conflict("session is busy"));
            }
        } else {
            Self::idle(&state)?;
        }
        let token = wd_tools::uuid::v4();
        state.reservation = Some((token.clone(), operation));
        Ok(token)
    }

    fn release(&self, reservation: &str) {
        let mut state = self.state.lock().unwrap();
        if state
            .reservation
            .as_ref()
            .is_some_and(|(id, _)| id == reservation)
        {
            state.reservation = None;
        }
    }

    fn commit_config(
        &self,
        token: &str,
        update: ConfigUpdateRequest,
    ) -> ControlResult<SessionConfig> {
        let mut state = self.state.lock().unwrap();
        Self::reserved(&state, token, SessionOperation::Configure)?;
        if let Some(workspace) = &update.workspace {
            if !std::path::Path::new(workspace).is_absolute() {
                return Err(SessionControlError::Invalid(
                    "workspace must be absolute".into(),
                ));
            }
        }
        if update
            .target
            .as_ref()
            .is_some_and(|target| target != &state.data.session_config.target)
            && state
                .data
                .session_config
                .channels
                .iter()
                .any(|v| matches!(v, ChannelBinding::Lark { .. }))
        {
            return Err(Self::conflict("Lark session target is immutable"));
        }
        if let Some(target) = update.target {
            if target != state.data.session_config.target {
                state.data.memory_view.history.clear();
                state.data.memory_view.history_offset = 0;
                state.data.memory_view.summary = None;
                state.history_loaded = false;
                state.data.session_config.target = target;
            }
        }
        if let Some(workspace) = update.workspace {
            state.data.session_config.workspace.root = workspace;
            state.data.session_config.workspace.project_files.clear();
            state.project_files_loaded = false;
        }
        if let Some(name) = update.name {
            state.data.session_config.name = name;
        }
        if let Some(options) = update.options {
            state.data.session_config.options = options;
        }
        state.data.session_config.updated_at = Timestamp::now();
        state.reservation = None;
        state.revision += 1;
        let config = state.data.session_config.clone();
        let _ = self
            .events
            .send(SessionControlEvent::ConfigUpdated(config.clone()));
        Ok(config)
    }

    fn commit_clear(&self, token: &str) -> ControlResult<()> {
        let mut state = self.state.lock().unwrap();
        Self::reserved(&state, token, SessionOperation::Clear)?;
        state.data.memory_view.history.clear();
        state.data.memory_view.history_offset = 0;
        state.data.memory_view.summary = None;
        state.history_loaded = true;
        state.reservation = None;
        state.revision += 1;
        Ok(())
    }

    fn archive(&self) -> ControlResult<SessionConfig> {
        let mut state = self.state.lock().unwrap();
        if state.data.session_config.lifecycle == SessionLifecycle::Archived {
            return Ok(state.data.session_config.clone());
        }
        Self::idle(&state)?;
        state.data.session_config.lifecycle = SessionLifecycle::Archived;
        state.data.session_config.updated_at = Timestamp::now();
        state.revision += 1;
        let config = state.data.session_config.clone();
        let _ = self
            .events
            .send(SessionControlEvent::ConfigUpdated(config.clone()));
        Ok(config)
    }

    fn read_summary(&self) -> SessionSummary {
        let state = self.state.lock().unwrap();
        let data = &state.data;
        SessionSummary {
            user_info: data.user_info.clone(),
            session_config: data.session_config.clone(),
            context: ContextSummary {
                memory: MemorySummary {
                    history_loaded: state.history_loaded,
                    history_offset: data.memory_view.history_offset,
                    history_count: data.memory_view.history.len(),
                    user_memories_loaded: state.user_memories_loaded,
                    user_memory_count: data.memory_view.user_memories.len(),
                    project_files_loaded: state.project_files_loaded,
                },
                latest_execution: data
                    .latest_execution_view
                    .as_ref()
                    .map(|v| ExecutionSummary {
                        execution_id: v.execution_id.clone(),
                        status: v.status,
                        started_at: v.started_at.clone(),
                        ended_at: v.ended_at.clone(),
                    }),
            },
            active_execution: state.active.as_ref().map(|_| {
                data.latest_execution_view
                    .as_ref()
                    .unwrap()
                    .execution_id
                    .clone()
            }),
        }
    }

    fn execution(&self, expected: Option<&ExecutionId>) -> ControlResult<ExecutionView> {
        let state = self.state.lock().unwrap();
        state
            .data
            .latest_execution_view
            .as_ref()
            .filter(|v| expected.is_none_or(|id| id == &v.execution_id))
            .cloned()
            .ok_or_else(|| SessionControlError::NotFound("execution is no longer retained".into()))
    }

    fn prepare(&self, settings: ExecutionSettings) -> ControlResult<ExecutionHandle> {
        self.prepare_with_environment(settings, ExecutionEnvironment::default())
    }

    fn prepare_with_environment(
        &self,
        settings: ExecutionSettings,
        environment: ExecutionEnvironment,
    ) -> ControlResult<ExecutionHandle> {
        let mut state = self.state.lock().unwrap();
        Self::idle(&state)?;
        if !std::path::Path::new(&settings.workspace).is_absolute() {
            return Err(SessionControlError::Invalid(
                "workspace must be absolute".into(),
            ));
        }
        let execution_id = ExecutionId::new(wd_tools::uuid::v4()).unwrap();
        let invocation_id = InvocationId::new(wd_tools::uuid::v4()).unwrap();
        let session = SessionHandle(self.weak.upgrade().unwrap());
        let mut scope = ContextScope::root(state.data.user_info.user_id.to_string());
        scope.workspace = Some(settings.workspace.clone().into());
        scope.execution_origin = environment.origin;
        scope.timer_execution = environment.timer;
        scope.execution = Some(ExecutionBinding {
            session: session.clone(),
            execution_id: execution_id.clone(),
            invocation_id: invocation_id.clone(),
            parent_invocation_id: None,
            definitions: Arc::default(),
        });
        let (agent_id, workflow_id) = match &settings.target {
            PlanTarget::Agent { agent_id } => (Some(agent_id.clone()), None),
            PlanTarget::Workflow { workflow_id } => (None, Some(workflow_id.clone())),
        };
        scope.agent_id = agent_id.clone();
        scope.workflow_id = workflow_id.clone();
        let ctx = self
            .engine
            .ctx_with_scope(scope)
            .map_err(|e| SessionControlError::Invalid(e.to_string()))?;
        state.data.latest_execution_view = Some(ExecutionView {
            execution_id: execution_id.clone(),
            target: settings.target.clone(),
            effective: settings,
            status: ExecutionStatus::Preparing,
            invocations: vec![InvocationView {
                invocation_id: invocation_id.clone(),
                parent_invocation_id: None,
                kind: InvocationKind::Plan,
                plan_id: PlanInstanceId::new(wd_tools::uuid::v4()).unwrap(),
                task_id: None,
                runtime_id: None,
                agent_id,
                workflow_id,
                node_id: None,
                model: None,
                status: InvocationStatus::Preparing,
            }],
            started_at: None,
            ended_at: None,
            outcome: None,
            inputs: vec![],
        });
        state.active = Some(ActiveExecution {
            ctx: ctx.clone(),
            pending_cleanup: HashSet::from([invocation_id.clone()]),
            resources: HashSet::new(),
            mailbox: VecDeque::new(),
            candidate: None,
            cancel_reason: None,
            initial_input: None,
        });
        self.publish(&state);
        Ok(ExecutionHandle {
            session,
            execution_id,
            invocation_id,
            ctx,
        })
    }

    fn running(&self, id: &ExecutionId) -> ControlResult<()> {
        let mut state = self.state.lock().unwrap();
        let view = Self::view(&mut state, id)?;
        if view.status == ExecutionStatus::Stopping {
            return Err(Self::conflict("execution was stopped during preparation"));
        }
        if view.status != ExecutionStatus::Preparing {
            return Err(Self::conflict("execution is not preparing"));
        }
        view.status = ExecutionStatus::Running;
        view.started_at = Some(Timestamp::now());
        self.publish(&state);
        Ok(())
    }

    fn steer(
        &self,
        id: &ExecutionId,
        request_id: RequestId,
        content: protocol::Content,
    ) -> ControlResult<InputRecord> {
        if content.is_empty()
            || content.iter().any(|block| match block {
                ContentBlock::Text { .. } => false,
                ContentBlock::Resource { resource } => {
                    !resource.media_type.starts_with("image/") || resource.uri.is_empty()
                }
                ContentBlock::Json { .. } => true,
            })
            || !content.iter().any(|block| match block {
                ContentBlock::Text { text } => !text.trim().is_empty(),
                ContentBlock::Resource { .. } => true,
                ContentBlock::Json { .. } => false,
            })
        {
            return Err(SessionControlError::Invalid(
                "Agent input requires nonempty text or image resources".into(),
            ));
        }
        let mut state = self.state.lock().unwrap();
        let view = Self::view(&mut state, id)?;
        if !matches!(
            view.status,
            ExecutionStatus::Running | ExecutionStatus::Waiting
        ) || !matches!(view.target, PlanTarget::Agent { .. })
        {
            return Err(Self::conflict("execution does not accept input"));
        }
        if let Some(record) = view.inputs.iter().find(|r| r.request_id == request_id) {
            return Ok(record.clone());
        }
        let record = InputRecord {
            request_id: request_id.clone(),
            execution_id: id.clone(),
            status: InputStatus::Queued,
            reason: None,
            received_at: Timestamp::now(),
            processed_at: None,
        };
        view.inputs.push(record.clone());
        state
            .active
            .as_mut()
            .unwrap()
            .mailbox
            .push_back(QueuedInput {
                request_id,
                content,
            });
        Ok(record)
    }

    fn take_inputs(&self, id: &ExecutionId) -> ControlResult<Vec<QueuedInput>> {
        let mut state = self.state.lock().unwrap();
        let view = Self::view(&mut state, id)?;
        if !matches!(
            view.status,
            ExecutionStatus::Running | ExecutionStatus::Waiting
        ) {
            return Err(Self::conflict("execution does not accept input"));
        }
        Ok(state.active.as_mut().unwrap().mailbox.drain(..).collect())
    }

    fn stop(&self, id: &ExecutionId, reason: String) -> ControlResult<bool> {
        let ctx = {
            let mut state = self.state.lock().unwrap();
            let view = Self::view(&mut state, id)?;
            if view.status.is_terminal()
                || matches!(
                    view.status,
                    ExecutionStatus::Finishing | ExecutionStatus::Stopping
                )
            {
                return Ok(false);
            }
            view.status = ExecutionStatus::Stopping;
            let active = state.active.as_mut().unwrap();
            active.cancel_reason = Some(reason.clone());
            let ctx = active.ctx.clone();
            self.publish(&state);
            ctx
        };
        ctx.request_cancel(reason.into())
            .map_err(|e| Self::conflict(&e.to_string()))?;
        Ok(true)
    }

    fn initial_input(&self, id: &ExecutionId) -> ControlResult<Option<RequestId>> {
        let mut state = self.state.lock().unwrap();
        Self::view(&mut state, id)?;
        Ok(state
            .active
            .as_ref()
            .and_then(|active| active.initial_input.clone()))
    }

    fn report(&self, id: &ExecutionId, report: ExecutionReport) -> ControlResult<()> {
        let mut state = self.state.lock().unwrap();
        let view = Self::view(&mut state, id)?;
        if view.status.is_terminal() {
            return Err(Self::conflict("execution has finished"));
        }
        match report {
            ExecutionReport::ResourceRegistered {
                invocation_id,
                resource_id,
            } => {
                if matches!(
                    view.status,
                    ExecutionStatus::Stopping | ExecutionStatus::Finishing
                ) || !view
                    .invocations
                    .iter()
                    .any(|i| i.invocation_id == invocation_id && !i.status.is_terminal())
                {
                    return Err(Self::conflict("resource invocation is inactive"));
                }
                state.active.as_mut().unwrap().resources.insert(resource_id);
            }
            ExecutionReport::ResourceReleased(resource_id) => {
                if !state
                    .active
                    .as_mut()
                    .unwrap()
                    .resources
                    .remove(&resource_id)
                {
                    return Err(Self::conflict("resource was not registered"));
                }
                self.commit_if_clean(&mut state);
            }
            ExecutionReport::InitialInputAccepted(request_id) => {
                if view.status != ExecutionStatus::Preparing || !view.inputs.is_empty() {
                    return Err(Self::conflict(
                        "initial input must be registered during preparation",
                    ));
                }
                view.inputs.push(InputRecord {
                    request_id: request_id.clone(),
                    execution_id: id.clone(),
                    status: InputStatus::Queued,
                    reason: None,
                    received_at: Timestamp::now(),
                    processed_at: None,
                });
                state.active.as_mut().unwrap().initial_input = Some(request_id);
            }
            ExecutionReport::MemoryLoaded(memories) => {
                state.data.memory_view.user_memories = memories;
                state.user_memories_loaded = true;
            }
            ExecutionReport::HistoryLoaded {
                partition,
                messages,
                offset,
            } => {
                if Self::default_partition(&state, &partition) {
                    state.data.memory_view.summary = messages
                        .iter()
                        .rev()
                        .find(|m| m.role == SessionMessageRole::Summary)
                        .map(|m| m.content.clone());
                    state.data.memory_view.history = messages;
                    state.data.memory_view.history_offset = offset;
                    state.history_loaded = true;
                }
            }
            ExecutionReport::HistoryAdded {
                invocation_id,
                partition,
                messages,
            } => {
                if !view
                    .invocations
                    .iter()
                    .any(|v| v.invocation_id == invocation_id)
                {
                    return Err(Self::conflict("history invocation was not registered"));
                }
                if Self::default_partition(&state, &partition) {
                    if state.history_loaded {
                        if let Some(message) = messages
                            .iter()
                            .rev()
                            .find(|m| m.role == SessionMessageRole::Summary)
                        {
                            state.data.memory_view.summary = Some(message.content.clone());
                        }
                        state.data.memory_view.history.extend(messages.clone());
                    }
                }
                let _ = self.events.send(SessionControlEvent::ConversationAppended(
                    ConversationAppended {
                        execution_id: id.clone(),
                        invocation_id,
                        partition,
                        messages,
                    },
                ));
            }
            ExecutionReport::Output {
                invocation_id,
                output_id,
                source,
                content,
            } => {
                if !view
                    .invocations
                    .iter()
                    .any(|v| v.invocation_id == invocation_id)
                {
                    return Err(Self::conflict("output invocation was not registered"));
                }
                let _ = self.events.send(SessionControlEvent::Output {
                    execution_id: id.clone(),
                    invocation_id,
                    output_id,
                    source,
                    content,
                });
            }
            ExecutionReport::DefinitionLoaded(definition) => {
                if matches!(
                    view.status,
                    ExecutionStatus::Stopping | ExecutionStatus::Finishing
                ) {
                    return Err(Self::conflict(
                        "execution is no longer preparing definitions",
                    ));
                }
                if let Some(current) = view
                    .effective
                    .definitions
                    .iter()
                    .find(|d| d.target == definition.target)
                {
                    if current != &definition {
                        return Err(Self::conflict("execution definition cannot change"));
                    }
                } else {
                    view.effective.definitions.push(definition);
                }
            }
            ExecutionReport::InvocationRegistered(invocation) => {
                if matches!(
                    view.status,
                    ExecutionStatus::Finishing | ExecutionStatus::Stopping
                ) || invocation
                    .parent_invocation_id
                    .as_ref()
                    .is_none_or(|parent| {
                        !view.invocations.iter().any(|v| &v.invocation_id == parent)
                    })
                    || view
                        .invocations
                        .iter()
                        .any(|v| v.invocation_id == invocation.invocation_id)
                {
                    return Err(Self::conflict("invalid or duplicate child invocation"));
                }
                view.invocations.push(invocation.clone());
                state
                    .active
                    .as_mut()
                    .unwrap()
                    .pending_cleanup
                    .insert(invocation.invocation_id.clone());
                let _ = self.events.send(SessionControlEvent::InvocationChanged {
                    execution_id: id.clone(),
                    invocation,
                });
            }
            ExecutionReport::InvocationChanged(invocation) => {
                let current = view
                    .invocations
                    .iter_mut()
                    .find(|v| v.invocation_id == invocation.invocation_id)
                    .ok_or_else(|| Self::conflict("invocation was not registered"))?;
                if current.status.is_terminal()
                    || current.parent_invocation_id != invocation.parent_invocation_id
                    || current.kind != invocation.kind
                    || (current.status != InvocationStatus::Preparing
                        && current.plan_id != invocation.plan_id)
                {
                    return Err(Self::conflict(
                        "invocation identity or terminal state cannot change",
                    ));
                }
                *current = invocation.clone();
                let _ = self.events.send(SessionControlEvent::InvocationChanged {
                    execution_id: id.clone(),
                    invocation,
                });
            }
            ExecutionReport::InputProcessed {
                request_id,
                applied,
                reason,
            } => {
                let record = view
                    .inputs
                    .iter_mut()
                    .find(|r| r.request_id == request_id)
                    .ok_or_else(|| Self::conflict("input was not accepted"))?;
                if record.status != InputStatus::Queued {
                    return Err(Self::conflict("input already has a processing decision"));
                }
                record.status = if applied {
                    InputStatus::Applied
                } else {
                    InputStatus::NotApplied
                };
                record.reason = reason;
                record.processed_at = Some(Timestamp::now());
                let _ = self
                    .events
                    .send(SessionControlEvent::InputProcessed(record.clone()));
            }
        }
        state.revision += 1;
        Ok(())
    }

    fn finishing(&self, id: &ExecutionId, outcome: ExecutionOutcome) -> ControlResult<()> {
        let mut state = self.state.lock().unwrap();
        let view = Self::view(&mut state, id)?;
        if view.status.is_terminal() || view.status == ExecutionStatus::Finishing {
            return Err(Self::conflict(
                "execution already has a completion candidate",
            ));
        }
        view.status = ExecutionStatus::Finishing;
        state.active.as_mut().unwrap().candidate = Some(outcome);
        self.publish(&state);
        self.commit_if_clean(&mut state);
        Ok(())
    }

    fn cleaned(&self, id: &ExecutionId, invocation_id: &InvocationId) -> ControlResult<()> {
        let mut state = self.state.lock().unwrap();
        Self::view(&mut state, id)?;
        let active = state
            .active
            .as_mut()
            .ok_or_else(|| Self::conflict("execution is inactive"))?;
        if !active.pending_cleanup.remove(invocation_id) {
            return Err(Self::conflict(
                "invocation cleanup was already reported or not registered",
            ));
        }
        self.commit_if_clean(&mut state);
        Ok(())
    }

    fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<SessionControlEvent> {
        self.events.subscribe()
    }
}
