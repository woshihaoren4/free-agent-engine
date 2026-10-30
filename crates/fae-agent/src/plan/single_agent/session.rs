use super::*;

#[derive(Debug, Clone)]
pub struct CommonSession {
    pub(super) inner: Arc<CommonSessionInner>,
    pub(crate) completion: Arc<CommonSessionCompletion>,
}

#[derive(Debug)]
pub(super) struct CommonSessionInner {
    user_id: StdRwLock<String>,
    channel: SessionOutputChannel,
    parent: Option<CommonSessionTarget>,
    pub(super) binding: RwLock<Option<SingleAgentBinding>>,
    state: StdMutex<CommonSessionState>,
    idle: Notify,
    pub(super) next_turn_id: AtomicU64,
}

#[derive(Debug, Clone)]
enum CommonSessionTarget {
    Workflow {
        session: CommonSession,
        workflow_id: String,
        node_id: String,
    },
    Agent {
        session: CommonSession,
        agent_name: String,
    },
}

#[derive(Debug, Default)]
struct CommonSessionState {
    active: bool,
    accepting_input: bool,
    cancel_requested: bool,
    pending_inputs: VecDeque<SessionInputData>,
}

#[derive(Debug)]
pub(crate) struct CommonSessionCompletion {
    result: StdMutex<Option<Result<Value, String>>>,
    notify: Notify,
    context: StdMutex<Option<crate::WeakCtx>>,
}

impl Default for CommonSessionCompletion {
    fn default() -> Self {
        Self {
            result: StdMutex::new(None),
            notify: Notify::new(),
            context: StdMutex::new(None),
        }
    }
}

impl CommonSession {
    pub fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub(crate) fn bind_context(&self, ctx: Ctx) {
        *self.completion.context.lock().unwrap() = Some(ctx.downgrade());
        *self.completion.result.lock().unwrap() = None;
    }
    pub(crate) fn new() -> Self {
        Self::new_with_user_id(DEFAULT_USER_ID)
    }

    pub(crate) fn new_with_user_id(user_id: impl Into<String>) -> Self {
        Self::new_with_parent(None, user_id.into())
    }

    pub fn user_id(&self) -> String {
        self.inner
            .user_id
            .read()
            .expect("session user ID lock poisoned")
            .clone()
    }

    pub(crate) fn set_user_id(&self, user_id: String) {
        *self
            .inner
            .user_id
            .write()
            .expect("session user ID lock poisoned") = user_id;
    }

    pub(super) fn new_in_workflow(
        session: CommonSession,
        workflow_id: impl Into<String>,
        node_id: impl Into<String>,
    ) -> Self {
        let user_id = session.user_id();
        Self::new_with_parent(
            Some(CommonSessionTarget::Workflow {
                session,
                workflow_id: workflow_id.into(),
                node_id: node_id.into(),
            }),
            user_id,
        )
    }

    pub(in crate::plan) fn new_in_agent(
        session: CommonSession,
        agent_name: impl Into<String>,
    ) -> Self {
        let user_id = session.user_id();
        Self::new_with_parent(
            Some(CommonSessionTarget::Agent {
                session,
                agent_name: agent_name.into(),
            }),
            user_id,
        )
    }

    fn new_with_parent(parent: Option<CommonSessionTarget>, user_id: String) -> Self {
        Self {
            inner: Arc::new(CommonSessionInner {
                user_id: StdRwLock::new(user_id),
                channel: SessionOutputChannel::new(),
                parent,
                binding: RwLock::new(None),
                state: StdMutex::new(CommonSessionState::default()),
                idle: Notify::new(),
                next_turn_id: AtomicU64::new(1),
            }),
            completion: Arc::new(CommonSessionCompletion::default()),
        }
    }

    pub(crate) fn emit_agent(
        &self,
        turn_id: u64,
        agent_name: impl Into<String>,
        source: impl Into<String>,
        data: SessionEventData,
    ) -> anyhow::Result<()> {
        let agent_name = agent_name.into();
        let source = source.into();
        self.emit(SessionEvent::single_agent_for(
            agent_name.clone(),
            turn_id,
            source.clone(),
            data.clone(),
        ))?;
        self.forward_agent_event(turn_id, agent_name, source, data)
    }

    fn forward_agent_event(
        &self,
        turn_id: u64,
        agent_name: String,
        source: String,
        data: SessionEventData,
    ) -> anyhow::Result<()> {
        let Some(parent) = &self.inner.parent else {
            return Ok(());
        };
        match parent {
            CommonSessionTarget::Workflow {
                session,
                workflow_id,
                node_id,
            } => session.emit(SessionEvent::agent_in_workflow(
                workflow_id.clone(),
                node_id.clone(),
                agent_name,
                turn_id,
                source,
                data,
            )),
            CommonSessionTarget::Agent {
                session,
                agent_name: parent_agent_name,
            } => {
                session.emit(SessionEvent::nested_agent(
                    parent_agent_name.clone(),
                    agent_name.clone(),
                    turn_id,
                    source.clone(),
                    data.clone(),
                ))?;
                session.forward_agent_event(turn_id, agent_name, source, data)
            }
        }
    }

    pub(crate) fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        let terminal = event.is_terminal().then(|| match &event.data {
            SessionEventData::NodeCompleted { output, .. } => Ok(output.clone()),
            SessionEventData::Failed { error } => Err(error.clone()),
            SessionEventData::Completed { content } => Ok(Value::String(content.clone())),
            _ => unreachable!("terminal session event has an unsupported payload"),
        });
        self.inner.channel.emit(event.into())?;
        if let Some(result) = terminal {
            self.completion.complete(result);
        }
        Ok(())
    }

    pub async fn result(&self) -> anyhow::Result<Value> {
        let ctx = self
            .completion
            .context
            .lock()
            .unwrap()
            .as_ref()
            .and_then(crate::WeakCtx::upgrade);
        if let Some(ctx) = ctx {
            return match &*ctx.terminal().await? {
                crate::ContextState::Succeeded { output } => output.json_snapshot(),
                crate::ContextState::Failed { error } => Err(anyhow::anyhow!("{}", error.message)),
                crate::ContextState::Cancelled { reason } => Err(anyhow::anyhow!("{}", reason)),
                _ => unreachable!(),
            };
        }
        loop {
            let notified = self.completion.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if let Some(result) = self.completion.result.lock().unwrap().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            notified.await;
        }
    }

    pub async fn set_mode(&self, mode: Option<&str>) -> anyhow::Result<()> {
        let mode = SingleAgentMode::parse(mode)?;
        anyhow::ensure!(
            !self
                .inner
                .state
                .lock()
                .expect("session state poisoned")
                .active,
            "cannot change single-agent mode while a turn is running"
        );
        let mut binding = self.inner.binding.write().await;
        let binding = binding
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("single-agent session is not bound to an engine"))?;
        binding.mode = mode;
        Ok(())
    }

    pub(super) async fn bind(&self, binding: SingleAgentBinding) -> anyhow::Result<u64> {
        let mut current = self.inner.binding.write().await;
        anyhow::ensure!(current.is_none(), "single-agent session is already bound");
        *current = Some(binding);
        self.activate_turn()
    }

    pub(in crate::plan) fn activate_turn(&self) -> anyhow::Result<u64> {
        let mut state = self.inner.state.lock().expect("session state poisoned");
        anyhow::ensure!(!state.active, "a turn is already running");
        state.active = true;
        state.accepting_input = true;
        Ok(self.inner.next_turn_id.fetch_add(1, Ordering::Relaxed))
    }

    pub(super) fn take_pending_inputs(&self) -> Vec<SessionInputData> {
        let mut state = self.inner.state.lock().expect("session state poisoned");
        state.pending_inputs.drain(..).collect()
    }

    pub(super) fn finish_or_take_pending(&self) -> Option<Vec<SessionInputData>> {
        let mut state = self.inner.state.lock().expect("session state poisoned");
        if state.pending_inputs.is_empty() {
            state.accepting_input = false;
            None
        } else {
            Some(state.pending_inputs.drain(..).collect())
        }
    }

    pub(in crate::plan) fn finish_turn(&self) {
        let mut state = self.inner.state.lock().expect("session state poisoned");
        state.active = false;
        state.accepting_input = false;
        state.cancel_requested = false;
        drop(state);
        self.inner.idle.notify_waiters();
    }

    pub(in crate::plan) fn cancel_requested(&self) -> bool {
        self.inner
            .state
            .lock()
            .expect("session state poisoned")
            .cancel_requested
    }

    pub(in crate::plan) fn abort_turn(&self) {
        let mut state = self.inner.state.lock().expect("session state poisoned");
        state.active = false;
        state.accepting_input = false;
        state.cancel_requested = false;
        state.pending_inputs.clear();
        drop(state);
        self.inner.idle.notify_waiters();
    }
}

impl CommonSessionCompletion {
    fn complete(&self, result: Result<Value, String>) {
        let mut completion = self.result.lock().unwrap();
        if completion.is_some() {
            return;
        }
        *completion = Some(result);
        drop(completion);
        self.notify.notify_waiters();
    }
}

#[async_trait::async_trait]
impl Session<SessionInput, SessionOutput> for CommonSession {
    async fn call(&self, input: SessionInput) -> anyhow::Result<()> {
        let (input, supplement) = match input {
            SessionInput::NewChat(input) => (input, false),
            SessionInput::Supplement(input) => (input, true),
        };
        anyhow::ensure!(!input.text.trim().is_empty(), "input text cannot be empty");
        let binding = self
            .inner
            .binding
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("single-agent session is not bound to an engine"))?;

        if supplement {
            let mut state = self.inner.state.lock().expect("session state poisoned");
            anyhow::ensure!(
                state.active && state.accepting_input,
                "there is no active conversation to supplement"
            );
            state.pending_inputs.push_back(input);
            return Ok(());
        }

        loop {
            let idle = self.inner.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            let should_wait = {
                let mut state = self.inner.state.lock().expect("session state poisoned");
                if !state.active {
                    state.active = true;
                    state.accepting_input = true;
                    false
                } else {
                    state.accepting_input = false;
                    state.cancel_requested = true;
                    state.pending_inputs.clear();
                    true
                }
            };
            if should_wait {
                if let Some(ctx) = self
                    .completion
                    .context
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(crate::WeakCtx::upgrade)
                {
                    let _ = ctx.set_state(crate::ContextState::cancelled(
                        "replaced by a new conversation",
                    ));
                }
                idle.await;
            } else {
                break;
            }
        }

        let turn_id = self.inner.next_turn_id.fetch_add(1, Ordering::Relaxed);
        let ctx = binding.engine.context();
        let mut scope = crate::ContextScope::root(binding.template.agent.user_id.clone());
        scope.agent_id = Some(binding.template.agent.name.clone());
        scope.session = Some(crate::ContextSession {
            session_id: binding.template.agent.session_id.clone(),
            session: self.clone(),
        });
        scope.turn_id = Some(turn_id);
        ctx.bind_scope(scope)?;
        self.bind_context(ctx.clone());
        ctx.set_state(crate::ContextState::Running)?;
        let images = input.images.unwrap_or_default();
        let plan = build_single_agent_plan(
            ctx.clone(),
            binding.template,
            input.text,
            images,
            turn_id,
            self.clone(),
            binding.mode,
        );
        let task = TaskReq {
            ctx,
            meta: TaskMeta {
                id: format!("single-agent-turn-{turn_id}"),
                ty: TaskType::Plan,
                ..Default::default()
            },
            req: plan,
        };

        if let Err(error) = binding.rt.spawn(task).await {
            self.abort_turn();
            return Err(error);
        }
        Ok(())
    }

    async fn answer(&self) -> anyhow::Result<Option<SessionOutput>> {
        Ok(self.inner.channel.answer().await)
    }
}
