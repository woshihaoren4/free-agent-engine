use crate::{CommonSession, Plan, RT, TaskMeta, common::AnyType};
use std::{
    fmt::Debug,
    ops::Deref,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

/// Core call metadata. Transport adapters own JSON decoding and authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCallMeta {
    pub plan_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCallMeta {
    pub runtime_id: String,
    pub ty: crate::TaskType,
}

#[derive(Debug, Clone)]
pub struct PlanDescriptor {
    pub id: String,
    pub env_type: String,
}

#[derive(Debug, Clone)]
pub struct RuntimeDescriptor {
    pub id: String,
    pub task_types: Vec<crate::TaskType>,
}

#[async_trait::async_trait]
pub trait Engine: Debug + Send + Sync + 'static {
    fn generation(&self) -> &str {
        "unregistered"
    }
    fn plans(&self) -> Vec<PlanDescriptor> {
        Vec::new()
    }
    fn runtimes(&self) -> Vec<RuntimeDescriptor> {
        self.rt().descriptors()
    }
    fn create_context(&self, scope: ContextScope) -> anyhow::Result<Ctx> {
        let ctx = self.context();
        ctx.bind_scope(scope)?;
        Ok(ctx)
    }
    async fn build_plan(
        &self,
        _ctx: Ctx,
        _meta: PlanCallMeta,
        _env: AnyType,
    ) -> anyhow::Result<Box<dyn Plan>> {
        anyhow::bail!("engine does not support Plan ID routes")
    }
    async fn fork_plan(
        &self,
        _ctx: &Ctx,
        _meta: PlanCallMeta,
        _env: AnyType,
    ) -> anyhow::Result<(Ctx, Box<dyn Plan>)> {
        anyhow::bail!("engine does not support Plan ID routes")
    }
    async fn execute_plan(
        &self,
        _ctx: Ctx,
        _meta: PlanCallMeta,
        _env: AnyType,
    ) -> anyhow::Result<()> {
        anyhow::bail!("engine does not support Plan execution by ID")
    }
    async fn execute_runtime(
        &self,
        _ctx: Ctx,
        _meta: RuntimeCallMeta,
        _req: AnyType,
    ) -> anyhow::Result<crate::TaskResponse> {
        anyhow::bail!("engine does not support Runtime execution by ID")
    }
    async fn select_on(
        &self,
        _ctx: &Ctx,
        _meta: RuntimeCallMeta,
        _cond: AnyType,
    ) -> anyhow::Result<AnyType> {
        anyhow::bail!("engine does not support Runtime queries by ID")
    }
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

/// The outer todo orchestration owns this ID, including all of its child steps.
#[derive(Debug)]
pub struct TodoPlanScope {
    pub plan_id: String,
    pub agent_id: String,
    pub user_id: String,
    pub user_input: String,
    lease: Mutex<Option<Box<dyn TodoPlanLease>>>,
}

pub trait TodoPlanLease: Debug + Send + Sync {}
impl<T: Debug + Send + Sync> TodoPlanLease for T {}

impl TodoPlanScope {
    pub fn new(plan_id: String, agent_id: String, user_id: String, user_input: String) -> Self {
        Self {
            plan_id,
            agent_id,
            user_id,
            user_input,
            lease: Mutex::new(None),
        }
    }

    /// Keep storage ownership alive for the entire orchestration and its child contexts.
    pub fn acquire_lease(
        &self,
        acquire: impl FnOnce() -> anyhow::Result<Box<dyn TodoPlanLease>>,
    ) -> anyhow::Result<()> {
        let mut lease = self.lease.lock().unwrap();
        if lease.is_none() {
            *lease = Some(acquire()?);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub enum TodoContextBinding {
    /// Task children inherit; independently delegated executions start unbound.
    #[default]
    Auto,
    Inherit,
    Clear,
    Bind(Arc<TodoPlanScope>),
}

impl TodoContextBinding {
    pub fn resolve(
        self,
        parent: &Option<Arc<TodoPlanScope>>,
        execution: bool,
    ) -> Option<Arc<TodoPlanScope>> {
        match self {
            Self::Auto if execution => None,
            Self::Auto | Self::Inherit => parent.clone(),
            Self::Clear => None,
            Self::Bind(scope) => Some(scope),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextScope {
    pub execution: Option<crate::ExecutionBinding>,
    pub scope_id: String,
    pub parent_scope_id: Option<String>,
    pub user: Arc<ContextUser>,
    pub session: Option<ContextSession>,
    pub agent_id: Option<String>,
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
    pub turn_id: Option<u64>,
    pub todo: Option<Arc<TodoPlanScope>>,
    /// Session-local execution directory, inherited by tools and child plans.
    pub workspace: Option<std::path::PathBuf>,
    pub execution_origin: Option<String>,
    pub timer_execution: Option<Arc<crate::TimerExecutionBinding>>,
}
impl ContextScope {
    pub fn root(user_id: impl Into<String>) -> Self {
        Self {
            execution: None,
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
            todo: None,
            workspace: None,
            execution_origin: None,
            timer_execution: None,
        }
    }
}

/// An execution child replaces execution ownership; a task child inherits it.
#[derive(Debug, Clone, Default)]
pub struct ContextChildSpec {
    pub timer_source: Option<crate::SingleAgentSource>,
    pub timer_workspace: Option<std::path::PathBuf>,
    pub workspace: Option<std::path::PathBuf>,
    pub session: Option<ContextSession>,
    pub agent_id: Option<String>,
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
    pub turn_id: Option<u64>,
    pub execution: bool,
    pub todo: TodoContextBinding,
}
impl ContextChildSpec {
    /// The inherited user always wins, including agents entered from workflows.
    pub fn timer_binding(
        &self,
        parent: &ContextScope,
    ) -> Option<Arc<crate::TimerExecutionBinding>> {
        if let Some(binding) = &parent.timer_execution {
            let mut binding = (**binding).clone();
            if let Some(source) = &self.timer_source {
                binding.source = source.clone();
                if let Some(agent) = &self.agent_id {
                    binding.owner.agent_id = agent.clone();
                }
            }
            return Some(Arc::new(binding));
        }
        Some(Arc::new(crate::TimerExecutionBinding {
            owner: crate::TimerOwner {
                agent_id: self.agent_id.clone()?,
                user_id: parent.user.user_id.clone(),
            },
            source: self.timer_source.clone()?,
            workspace: self.timer_workspace.clone()?,
        }))
    }
}

pub type ContextCleanupFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;

pub struct ContextCleanup(pub ContextCleanupFuture);
impl Debug for ContextCleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContextCleanup")
    }
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
            .or_else(|| value.is::<()>().then_some(serde_json::Value::Null))
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
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Stage a result until execution cleanup has completed.
    fn submit_output(&self, output: ContextOutput) -> anyhow::Result<()> {
        self.set_state(ContextState::Succeeded { output })?;
        Ok(())
    }
    fn discard_output(&self) {}
    fn publish_output(&self) -> anyhow::Result<()> {
        if !self.state().current().is_terminal() {
            self.set_state(ContextState::succeeded(()))?;
        }
        Ok(())
    }
    fn register_cleanup(&self, _cleanup: ContextCleanup) -> anyhow::Result<()> {
        anyhow::bail!("context does not support execution cleanup")
    }
    fn cleanup(&self) -> ContextCleanupFuture {
        Box::pin(async { Ok(()) })
    }
    /// Cancellation is a signal; it does not commit a terminal result.
    fn cancellation_signal(&self) -> Option<watch::Receiver<Option<Arc<str>>>> {
        None
    }
    fn request_cancel(&self, reason: Arc<str>) -> anyhow::Result<()> {
        self.set_state(ContextState::Cancelled { reason })?;
        Ok(())
    }
    fn plugin_execution(&self) -> Option<Arc<crate::plugin::PluginExecutionBinding>> {
        None
    }
    fn bind_plugin_execution(&self, _binding: Arc<crate::plugin::PluginExecutionBinding>) {}
    /// Host-only short synchronous admission; implementations can serialize it
    /// with terminal publication. The action must never call back into this Ctx.
    fn with_active(&self, action: &mut dyn FnMut() -> anyhow::Result<()>) -> anyhow::Result<()> {
        anyhow::ensure!(!self.state().current().is_terminal(), "ContextFinished");
        action()
    }
    /// 获取当前上下文所属的引擎，用于访问运行时、构建计划或准备子计划。
    fn get_engine(&self) -> EngineRef;

    /// Return the session bound to this scope, without consulting its current turn.
    fn get_session(&self) -> anyhow::Result<Option<CommonSession>> {
        Ok(self
            .scope()?
            .session
            .as_ref()
            .map(|binding| binding.session.clone()))
    }

    /// 获取已绑定的作用域，读取用户、会话、执行归属及工作目录等信息；未绑定时返回错误。
    fn scope(&self) -> anyhow::Result<Arc<ContextScope>>;
    /// Whether a scope belongs to this Context's subtree, including itself.
    fn owns_scope(&self, scope_id: &str) -> bool {
        self.scope().is_ok_and(|scope| scope.scope_id == scope_id)
    }

    /// 为尚处于 Created 状态的上下文一次性绑定根作用域。
    /// 作用域必须通过身份校验且不能指定父作用域；重复绑定或状态不符时返回错误。
    fn bind_scope(&self, scope: ContextScope) -> anyhow::Result<()>;

    /// 按指定规则创建子上下文，生成新的作用域 ID 并关联父作用域，始终继承用户身份。
    /// 任务子上下文默认继承执行归属，独立执行子上下文按 spec 重新指定归属；
    /// 子上下文共享引擎和执行记录存储，但拥有独立的 Created 状态。父作用域未绑定或已终结时返回错误。
    fn fork(&self, spec: ContextChildSpec) -> anyhow::Result<Ctx>;

    /// 订阅当前上下文的执行状态，可读取最新状态并异步等待变化，不消费成功结果。
    fn state(&self) -> ContextStateView;

    /// 更新执行状态并通知订阅者，返回 Applied 或 Unchanged；非法迁移或终态后的写入返回错误。
    /// Created 可转为 Running、Failed 或 Cancelled，Running 可转为任一终态；
    /// 重复设置 Created 或 Running 不触发更新，失败或取消会递归取消尚未终结的子上下文。
    fn set_state(&self, state: ContextState) -> anyhow::Result<ContextStateUpdate>;

    /// 为当前作用域追加任务生命周期记录，补充记录 ID、全树递增序号和时间戳。
    /// 结束阶段可关联同一任务的开始记录并补充耗时；返回记录 ID，未绑定作用域、
    /// 指定的父记录不存在或共享存储容量已满时返回错误。
    fn append_stack(&self, entry: ContextStackDraft) -> anyhow::Result<StackEntryId>;

    /// 按任务、计划、执行器、阶段及序号筛选执行记录，并按序号升序返回。
    /// 默认只查询当前作用域，tree 为 true 时包含后代作用域；
    /// after_sequence 为排他下界，limit 未指定时最多返回 256 条。
    fn stacks(&self, query: ContextStackQuery) -> anyhow::Result<Vec<ContextStack>>;
}
#[derive(Debug)]
pub struct ContextNull;
impl Context for ContextNull {
    fn get_session(&self) -> anyhow::Result<Option<CommonSession>> {
        Ok(None)
    }

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
    pub fn submit_result<T: Send + 'static>(&self, value: T) -> anyhow::Result<()> {
        self.submit_output(ContextOutput::new(Box::new(value)))
    }
    /// Fix loaded definition data once per Execution, shared by child Invocations.
    pub async fn definition<T, F>(
        &self,
        key: String,
        target: crate::PlanTarget,
        source: Option<String>,
        load: F,
    ) -> anyhow::Result<T>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
        F: std::future::Future<Output = anyhow::Result<T>>,
    {
        use sha2::{Digest, Sha256};
        let Some(binding) = self.scope().ok().and_then(|scope| scope.execution.clone()) else {
            return load.await;
        };
        let mut definitions = binding.definitions.lock().await;
        if let Some(value) = definitions.get(&key) {
            return Ok(serde_json::from_value(value.clone())?);
        }
        let loaded = load.await?;
        let value = serde_json::to_value(&loaded)?;
        let content_fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?));
        binding.session.report(
            &binding.execution_id,
            crate::ExecutionReport::DefinitionLoaded(crate::DefinitionRef {
                target,
                source,
                content_fingerprint,
            }),
        )?;
        definitions.insert(key, value);
        Ok(loaded)
    }
    pub fn execution_output(
        &self,
        output_id: String,
        content: crate::protocol::Content,
    ) -> anyhow::Result<()> {
        let binding = self
            .scope()?
            .execution
            .clone()
            .ok_or_else(|| anyhow::anyhow!("context has no session execution"))?;
        binding.session.report(
            &binding.execution_id,
            crate::ExecutionReport::Output {
                invocation_id: binding.invocation_id,
                output_id,
                source: crate::protocol::OutputSource::Runtime,
                content,
            },
        )?;
        Ok(())
    }
    pub fn cancel_requested(&self) -> bool {
        self.cancellation_signal()
            .is_some_and(|signal| signal.borrow().is_some())
            || self.state().current().is_cancelled()
    }

    /// Register actual dispatch, rather than treating all debug scopes as calls.
    pub fn invocation_started(&self, meta: &TaskMeta) -> anyhow::Result<()> {
        self.invocation_dispatch(meta, None)
    }

    pub fn invocation_plan_started(&self, meta: &TaskMeta, plan_id: &str) -> anyhow::Result<()> {
        self.invocation_dispatch(meta, Some(plan_id))
    }

    fn invocation_dispatch(&self, meta: &TaskMeta, plan_id: Option<&str>) -> anyhow::Result<()> {
        let Some(binding) = self.scope().ok().and_then(|s| s.execution.clone()) else {
            return Ok(());
        };
        let view = binding.session.execution(Some(&binding.execution_id))?;
        let scope = self.scope()?;
        let mut invocation = view
            .invocations
            .iter()
            .find(|v| v.invocation_id == binding.invocation_id)
            .cloned()
            .unwrap_or(crate::InvocationView {
                invocation_id: binding.invocation_id.clone(),
                parent_invocation_id: binding.parent_invocation_id.clone(),
                kind: if meta.ty == crate::TaskType::Plan {
                    crate::InvocationKind::Plan
                } else {
                    crate::InvocationKind::Runtime
                },
                plan_id: crate::PlanInstanceId::new(if let Some(plan_id) = plan_id {
                    plan_id.to_owned()
                } else if meta.plan_run_id.is_empty() {
                    meta.id.clone()
                } else {
                    meta.plan_run_id.clone()
                })?,
                task_id: Some(crate::TaskId::new(meta.id.clone())?),
                runtime_id: Some(meta.executor.clone()),
                agent_id: scope.agent_id.clone(),
                workflow_id: scope.workflow_id.clone(),
                node_id: scope.node_id.clone(),
                model: None,
                status: crate::InvocationStatus::Preparing,
            });
        if !view
            .invocations
            .iter()
            .any(|v| v.invocation_id == binding.invocation_id)
        {
            binding.session.report(
                &binding.execution_id,
                crate::ExecutionReport::InvocationRegistered(invocation.clone()),
            )?;
        }
        invocation.status = crate::InvocationStatus::Running;
        if let Some(plan_id) = plan_id {
            invocation.plan_id = crate::PlanInstanceId::new(plan_id)?;
        } else if !meta.plan_run_id.is_empty() {
            invocation.plan_id = crate::PlanInstanceId::new(meta.plan_run_id.clone())?;
        }
        invocation.task_id = Some(crate::TaskId::new(meta.id.clone())?);
        invocation.runtime_id = (!meta.executor.is_empty()).then(|| meta.executor.clone());
        invocation.agent_id = scope.agent_id.clone();
        invocation.workflow_id = scope.workflow_id.clone();
        invocation.node_id = scope.node_id.clone();
        binding.session.report(
            &binding.execution_id,
            crate::ExecutionReport::InvocationChanged(invocation),
        )?;
        Ok(())
    }

    pub fn initial_input_applied(&self) -> anyhow::Result<()> {
        let Some(binding) = self.scope().ok().and_then(|scope| scope.execution.clone()) else {
            return Ok(());
        };
        if binding.parent_invocation_id.is_some() {
            return Ok(());
        }
        if let Some(request_id) = binding.session.initial_input(&binding.execution_id)? {
            binding.session.report(
                &binding.execution_id,
                crate::ExecutionReport::InputProcessed {
                    request_id,
                    applied: true,
                    reason: None,
                },
            )?;
        }
        Ok(())
    }

    pub fn invocation_model(&self, model: crate::ModelRef) -> anyhow::Result<()> {
        let Some(binding) = self.scope().ok().and_then(|scope| scope.execution.clone()) else {
            return Ok(());
        };
        let view = binding.session.execution(Some(&binding.execution_id))?;
        let Some(mut invocation) = view
            .invocations
            .into_iter()
            .find(|invocation| invocation.invocation_id == binding.invocation_id)
        else {
            return Ok(());
        };
        invocation.model = Some(model);
        binding.session.report(
            &binding.execution_id,
            crate::ExecutionReport::InvocationChanged(invocation),
        )?;
        Ok(())
    }

    pub fn invocation_finished(&self) -> anyhow::Result<()> {
        let Some(binding) = self.scope().ok().and_then(|s| s.execution.clone()) else {
            return Ok(());
        };
        let view = binding.session.execution(Some(&binding.execution_id))?;
        let Some(mut invocation) = view
            .invocations
            .iter()
            .find(|v| v.invocation_id == binding.invocation_id)
            .cloned()
        else {
            return Ok(());
        };
        if invocation.status.is_terminal() {
            return Ok(());
        }
        invocation.status = match &*self.state().current() {
            ContextState::Succeeded { .. } => crate::InvocationStatus::Succeeded,
            ContextState::Failed { .. } => crate::InvocationStatus::Failed,
            _ => crate::InvocationStatus::Cancelled,
        };
        binding.session.report(
            &binding.execution_id,
            crate::ExecutionReport::InvocationChanged(invocation),
        )?;
        binding
            .session
            .cleaned(&binding.execution_id, &binding.invocation_id)?;
        Ok(())
    }
    pub fn same_context(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// Prepare an independent tool scope without mutating the caller's scope.
    pub fn with_todo_plan(&self, plan_id: String, user_input: String) -> anyhow::Result<Self> {
        let scope = self.scope()?;
        let agent_id = scope
            .agent_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("todo requires agent ownership in context"))?;
        let ctx = self.fork(ContextChildSpec {
            todo: TodoContextBinding::Bind(Arc::new(TodoPlanScope::new(
                plan_id,
                agent_id,
                scope.user.user_id.clone(),
                user_input,
            ))),
            ..Default::default()
        })?;
        ctx.set_state(ContextState::Running)?;
        Ok(ctx)
    }

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
        if let Some(mut signal) = self.cancellation_signal() {
            loop {
                if let Some(reason) = signal.borrow().clone() {
                    return reason;
                }
                if signal.changed().await.is_err() {
                    return std::future::pending().await;
                }
            }
        }
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
            scope.timer_execution = spec.timer_binding(&parent);
            scope.workspace = parent.workspace.clone().or(spec.workspace);
            scope.todo = spec.todo.resolve(&parent.todo, spec.execution);
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
