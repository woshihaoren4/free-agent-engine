use super::*;

/// Local Plan output transport. Execution commands belong to SessionHandle.
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
    active: StdMutex<bool>,
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
pub(crate) struct CommonSessionCompletion {
    result: StdMutex<Option<Result<Value, String>>>,
    notify: Notify,
    context: StdMutex<Option<crate::WeakCtx>>,
}

impl CommonSession {
    pub fn active_state(&self) -> anyhow::Result<crate::ContextStateView> {
        let ctx = self
            .completion
            .context
            .lock()
            .unwrap()
            .as_ref()
            .and_then(crate::WeakCtx::upgrade)
            .ok_or_else(|| anyhow::anyhow!("plan_not_active"))?;
        Ok(ctx.state())
    }

    pub async fn drain_outputs(&self) -> Vec<SessionOutput> {
        self.inner.channel.drain().await
    }

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
            .expect("Plan user ID lock poisoned")
            .clone()
    }

    pub(crate) fn set_user_id(&self, user_id: String) {
        *self
            .inner
            .user_id
            .write()
            .expect("Plan user ID lock poisoned") = user_id;
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
                active: StdMutex::new(false),
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
            _ => unreachable!("unsupported terminal Plan event"),
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

    pub(crate) fn activate_turn(&self) -> anyhow::Result<u64> {
        let mut active = self.inner.active.lock().expect("Plan state poisoned");
        anyhow::ensure!(!*active, "Plan is already running");
        *active = true;
        Ok(self.inner.next_turn_id.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn finish_turn(&self) {
        *self.inner.active.lock().expect("Plan state poisoned") = false;
    }

    pub(in crate::plan) fn abort_turn(&self) {
        self.finish_turn();
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
impl SessionIo<SessionOutput> for CommonSession {
    async fn answer(&self) -> anyhow::Result<Option<SessionOutput>> {
        Ok(self.inner.channel.answer().await)
    }
}
