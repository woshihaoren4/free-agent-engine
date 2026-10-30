use crate::{CommonSession, Plan, RT, TaskMeta, common::AnyType};
use std::{
    fmt::Debug,
    ops::Deref,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

#[async_trait::async_trait]
pub trait Engine: Debug + Send + Sync + 'static {
    fn rt(&self) -> RT;
    fn context(&self) -> Ctx {
        Ctx::null()
    }
    async fn call(&self, ctx: Ctx, ty: String, env: AnyType) -> anyhow::Result<Box<dyn Plan>>;
    async fn fork_call(
        &self,
        _ctx: &Ctx,
        _ty: String,
        _env: AnyType,
    ) -> anyhow::Result<(Ctx, Box<dyn Plan>)> {
        anyhow::bail!("engine does not support child plan preparation")
    }
}
pub type EngineRef = Arc<dyn Engine>;

#[derive(Debug)]
struct EngineNull;
#[async_trait::async_trait]
impl Engine for EngineNull {
    fn rt(&self) -> RT {
        RT::null()
    }
    async fn call(&self, _: Ctx, _: String, _: AnyType) -> anyhow::Result<Box<dyn Plan>> {
        anyhow::bail!("null engine cannot build plans")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextUser {
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct ContextSession {
    pub session_id: String,
    pub session: CommonSession,
}

#[derive(Debug, Clone)]
pub struct ContextScope {
    pub scope_id: String,
    pub parent_scope_id: Option<String>,
    pub user: Arc<ContextUser>,
    pub session: Option<ContextSession>,
    pub agent_id: Option<String>,
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
    pub turn_id: Option<u64>,
}
impl ContextScope {
    pub fn root(user_id: impl Into<String>) -> Self {
        Self {
            scope_id: wd_tools::uuid::v4(),
            parent_scope_id: None,
            user: Arc::new(ContextUser {
                user_id: user_id.into(),
            }),
            session: None,
            agent_id: None,
            workflow_id: None,
            node_id: None,
            turn_id: None,
        }
    }
}

/// An execution child replaces execution ownership; a task child inherits it.
#[derive(Debug, Clone, Default)]
pub struct ContextChildSpec {
    pub session: Option<ContextSession>,
    pub agent_id: Option<String>,
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
    pub turn_id: Option<u64>,
    pub execution: bool,
}

#[derive(Clone)]
pub struct ContextOutput {
    value: Arc<Mutex<Option<AnyType>>>,
    session_snapshot: Option<Arc<serde_json::Value>>,
}
impl Debug for ContextOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextOutput")
            .field("consumed", &self.value.lock().unwrap().is_none())
            .finish()
    }
}
impl ContextOutput {
    pub fn new(value: AnyType) -> Self {
        let session_snapshot = value
            .downcast_ref::<serde_json::Value>()
            .cloned()
            .or_else(|| {
                value
                    .downcast_ref::<String>()
                    .map(|value| value.clone().into())
            })
            .map(Arc::new);
        Self {
            value: Arc::new(Mutex::new(Some(value))),
            session_snapshot,
        }
    }
    pub fn take<T: Send + 'static>(&self) -> anyhow::Result<T> {
        let mut slot = self.value.lock().unwrap();
        let value = slot
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("context result has already been consumed"))?;
        anyhow::ensure!(
            value.is::<T>(),
            "context result type does not match `{}`",
            std::any::type_name::<T>()
        );
        Ok(*slot.take().unwrap().downcast::<T>().unwrap())
    }
    pub fn json_snapshot(&self) -> anyhow::Result<serde_json::Value> {
        self.session_snapshot
            .as_ref()
            .map(|value| (**value).clone())
            .ok_or_else(|| anyhow::anyhow!("context output is not a session JSON result"))
    }
}
#[derive(Debug, Clone)]
pub struct ContextFailure {
    pub message: String,
}
#[derive(Debug, Clone)]
pub enum ContextState {
    Created,
    Running,
    Succeeded { output: ContextOutput },
    Failed { error: Arc<ContextFailure> },
    Cancelled { reason: Arc<str> },
}
impl ContextState {
    pub fn succeeded<T: Send + 'static>(value: T) -> Self {
        Self::Succeeded {
            output: ContextOutput::new(Box::new(value)),
        }
    }
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            error: Arc::new(ContextFailure {
                message: message.into(),
            }),
        }
    }
    pub fn cancelled(reason: impl Into<Arc<str>>) -> Self {
        Self::Cancelled {
            reason: reason.into(),
        }
    }
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextStateUpdate {
    Applied,
    Unchanged,
}
#[derive(Debug, Clone)]
pub struct ContextStateView(watch::Receiver<Arc<ContextState>>);
impl ContextStateView {
    pub fn new(receiver: watch::Receiver<Arc<ContextState>>) -> Self {
        Self(receiver)
    }
    pub fn current(&self) -> Arc<ContextState> {
        self.0.borrow().clone()
    }
    pub async fn changed(&mut self) -> anyhow::Result<()> {
        self.0.changed().await.map_err(Into::into)
    }
}

pub type StackEntryId = u64;
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContextStackPhase {
    Dispatched,
    Started,
    Succeeded,
    Failed,
    Cancelled,
    CleanupFinished,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextStack {
    pub entry_id: StackEntryId,
    pub parent_entry_id: Option<StackEntryId>,
    pub sequence: u64,
    pub scope_id: String,
    pub task_meta: TaskMeta,
    pub phase: ContextStackPhase,
    pub timestamp: String,
    pub elapsed_ms: Option<u64>,
    pub message: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ContextStackDraft {
    pub task_meta: TaskMeta,
    pub phase: ContextStackPhase,
    pub parent_entry_id: Option<StackEntryId>,
    pub elapsed_ms: Option<u64>,
    pub message: Option<String>,
}
impl ContextStackDraft {
    pub fn new(task_meta: TaskMeta, phase: ContextStackPhase) -> Self {
        Self {
            task_meta,
            phase,
            parent_entry_id: None,
            elapsed_ms: None,
            message: None,
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct ContextStackQuery {
    pub tree: bool,
    pub task_id: Option<String>,
    pub plan_id: Option<String>,
    pub executor: Option<String>,
    pub phase: Option<ContextStackPhase>,
    pub after_sequence: Option<u64>,
    pub limit: Option<usize>,
}

pub trait Context: Debug + Send + Sync + 'static {
    fn get_engine(&self) -> EngineRef;
    fn scope(&self) -> anyhow::Result<Arc<ContextScope>>;
    fn bind_scope(&self, scope: ContextScope) -> anyhow::Result<()>;
    fn fork(&self, spec: ContextChildSpec) -> anyhow::Result<Ctx>;
    fn state(&self) -> ContextStateView;
    fn set_state(&self, state: ContextState) -> anyhow::Result<ContextStateUpdate>;
    fn append_stack(&self, entry: ContextStackDraft) -> anyhow::Result<StackEntryId>;
    fn stacks(&self, query: ContextStackQuery) -> anyhow::Result<Vec<ContextStack>>;
}
#[derive(Debug)]
pub struct ContextNull;
impl Context for ContextNull {
    fn get_engine(&self) -> EngineRef {
        Arc::new(EngineNull)
    }
    fn scope(&self) -> anyhow::Result<Arc<ContextScope>> {
        anyhow::bail!("context scope is not bound")
    }
    fn bind_scope(&self, _: ContextScope) -> anyhow::Result<()> {
        anyhow::bail!("null context cannot bind a scope")
    }
    fn fork(&self, _: ContextChildSpec) -> anyhow::Result<Ctx> {
        anyhow::bail!("null context cannot fork")
    }
    fn state(&self) -> ContextStateView {
        ContextStateView::new(watch::channel(Arc::new(ContextState::Created)).1)
    }
    fn set_state(&self, _: ContextState) -> anyhow::Result<ContextStateUpdate> {
        anyhow::bail!("null context has no execution state")
    }
    fn append_stack(&self, _: ContextStackDraft) -> anyhow::Result<StackEntryId> {
        anyhow::bail!("null context has no stack")
    }
    fn stacks(&self, _: ContextStackQuery) -> anyhow::Result<Vec<ContextStack>> {
        Ok(Vec::new())
    }
}
#[derive(Debug, Clone)]
pub struct Ctx(Arc<dyn Context>);
#[derive(Debug, Clone)]
pub struct WeakCtx(std::sync::Weak<dyn Context>);
impl WeakCtx {
    pub fn upgrade(&self) -> Option<Ctx> {
        self.0.upgrade().map(Ctx)
    }
}
impl Ctx {
    pub fn downgrade(&self) -> WeakCtx {
        WeakCtx(Arc::downgrade(&self.0))
    }
    pub fn new(ctx: Arc<dyn Context>) -> Self {
        Self(ctx)
    }
    pub(crate) fn null() -> Self {
        Self(Arc::new(ContextNull))
    }
    pub async fn cancellation(&self) -> Arc<str> {
        let mut view = self.state();
        loop {
            if let ContextState::Cancelled { reason } = &*view.current() {
                return reason.clone();
            }
            if view.changed().await.is_err() {
                return std::future::pending().await;
            }
        }
    }
    pub async fn terminal(&self) -> anyhow::Result<Arc<ContextState>> {
        let mut view = self.state();
        loop {
            let state = view.current();
            if state.is_terminal() {
                return Ok(state);
            }
            view.changed().await?;
        }
    }
    pub async fn result<T: Send + 'static>(&self) -> anyhow::Result<T> {
        match &*self.terminal().await? {
            ContextState::Succeeded { output } => output.take(),
            ContextState::Failed { error } => anyhow::bail!("{}", error.message),
            ContextState::Cancelled { reason } => anyhow::bail!("{}", reason),
            _ => unreachable!(),
        }
    }
}
impl Deref for Ctx {
    type Target = dyn Context;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

/// Stateful protocol fixture. Identity-dependent execution must not use ContextNull.
#[cfg(test)]
pub(crate) fn test_context(scope: Option<ContextScope>) -> Ctx {
    #[derive(Debug)]
    struct TestContext {
        scope: Mutex<Option<Arc<ContextScope>>>,
        state: Mutex<watch::Sender<Arc<ContextState>>>,
    }
    impl Context for TestContext {
        fn get_engine(&self) -> EngineRef {
            Arc::new(EngineNull)
        }
        fn scope(&self) -> anyhow::Result<Arc<ContextScope>> {
            self.scope
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("context scope is not bound"))
        }
        fn bind_scope(&self, scope: ContextScope) -> anyhow::Result<()> {
            let mut slot = self.scope.lock().unwrap();
            anyhow::ensure!(slot.is_none(), "context scope is already bound");
            *slot = Some(Arc::new(scope));
            Ok(())
        }
        fn fork(&self, spec: ContextChildSpec) -> anyhow::Result<Ctx> {
            let parent = self.scope()?;
            let mut scope = (*parent).clone();
            scope.scope_id = wd_tools::uuid::v4();
            scope.parent_scope_id = Some(parent.scope_id.clone());
            if spec.execution {
                scope.session = spec.session;
                scope.agent_id = spec.agent_id;
                scope.workflow_id = spec.workflow_id;
                scope.node_id = spec.node_id;
                scope.turn_id = spec.turn_id;
            } else {
                if spec.session.is_some() {
                    scope.session = spec.session;
                }
                if spec.agent_id.is_some() {
                    scope.agent_id = spec.agent_id;
                }
                if spec.workflow_id.is_some() {
                    scope.workflow_id = spec.workflow_id;
                }
                if spec.node_id.is_some() {
                    scope.node_id = spec.node_id;
                }
                if spec.turn_id.is_some() {
                    scope.turn_id = spec.turn_id;
                }
            }
            Ok(test_context(Some(scope)))
        }
        fn state(&self) -> ContextStateView {
            ContextStateView::new(self.state.lock().unwrap().subscribe())
        }
        fn set_state(&self, state: ContextState) -> anyhow::Result<ContextStateUpdate> {
            let sender = self.state.lock().unwrap();
            anyhow::ensure!(
                !sender.borrow().is_terminal(),
                "context is already terminal"
            );
            sender.send_replace(Arc::new(state));
            Ok(ContextStateUpdate::Applied)
        }
        fn append_stack(&self, _: ContextStackDraft) -> anyhow::Result<StackEntryId> {
            Ok(1)
        }
        fn stacks(&self, _: ContextStackQuery) -> anyhow::Result<Vec<ContextStack>> {
            Ok(Vec::new())
        }
    }
    Ctx::new(Arc::new(TestContext {
        scope: Mutex::new(scope.map(Arc::new)),
        state: Mutex::new(watch::channel(Arc::new(ContextState::Created)).0),
    }))
}
