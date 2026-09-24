use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    CommonSession, Ctx, Plan, PlanBuilderWithEnv, PlanNext, Session, SessionEvent,
    SessionEventData, SessionResponse, SingleAgentEnv, TaskMeta, TaskReq, TaskRequest, TaskResp,
    TaskResponse, TaskType, ToolRequest, ToolRespItem, ToolResponse, WorkflowAction,
    WorkflowActionRequest, WorkflowActionResponse, WorkflowEnv, WorkflowHook, WorkflowHookBuilder,
    WorkflowHookContext, WorkflowHookPhase, WorkflowMetadata, WorkflowNode, WorkflowValues,
    to_plan_ty,
};

use crate::hook::workflow_hook::CompositeWorkflowHook;

use super::builder::requires_dag_execution;

#[async_trait::async_trait]
pub trait WorkflowMetadataLoader: std::fmt::Debug + Send + Sync + 'static {
    async fn load(&self, workflow_id: &str) -> anyhow::Result<WorkflowMetadata>;
}

#[derive(Debug, Clone)]
pub struct FAEWorkflowMetadataLoader {
    workflows: Arc<RwLock<HashMap<String, WorkflowMetadata>>>,
    home_dir: PathBuf,
}

impl Default for FAEWorkflowMetadataLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl FAEWorkflowMetadataLoader {
    pub fn new() -> Self {
        Self::with_home_dir(default_fae_home())
    }

    pub fn with_home_dir(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            workflows: Arc::default(),
            home_dir: home_dir.into(),
        }
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub fn add(&self, metadata: WorkflowMetadata) -> anyhow::Result<Option<WorkflowMetadata>> {
        metadata.validate()?;
        let id = metadata.id.clone();
        Ok(self
            .workflows
            .write()
            .map_err(|_| anyhow::anyhow!("workflow metadata registry lock is poisoned"))?
            .insert(id, metadata))
    }

    pub fn remove(&self, id: &str) -> anyhow::Result<Option<WorkflowMetadata>> {
        Ok(self
            .workflows
            .write()
            .map_err(|_| anyhow::anyhow!("workflow metadata registry lock is poisoned"))?
            .remove(id))
    }
}

#[async_trait::async_trait]
impl WorkflowMetadataLoader for FAEWorkflowMetadataLoader {
    async fn load(&self, workflow_id: &str) -> anyhow::Result<WorkflowMetadata> {
        if let Some(metadata) = self
            .workflows
            .read()
            .map_err(|_| anyhow::anyhow!("workflow metadata registry lock is poisoned"))?
            .get(workflow_id)
            .cloned()
        {
            return Ok(metadata);
        }

        validate_workflow_id(workflow_id)?;
        let path = self
            .home_dir
            .join("workflows")
            .join(format!("{workflow_id}.json"));
        let metadata = WorkflowMetadata::load_json(&path).await.map_err(|error| {
            anyhow::anyhow!(
                "load workflow `{workflow_id}` from `{}`: {error}",
                path.display()
            )
        })?;
        anyhow::ensure!(
            metadata.id == workflow_id,
            "workflow file `{}` contains id `{}`, expected `{workflow_id}`",
            path.display(),
            metadata.id
        );
        Ok(metadata)
    }
}

fn validate_workflow_id(workflow_id: &str) -> anyhow::Result<()> {
    let mut components = Path::new(workflow_id).components();
    anyhow::ensure!(
        !workflow_id.is_empty()
            && matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none(),
        "workflow id must be a single non-empty path component"
    );
    Ok(())
}

fn default_fae_home() -> PathBuf {
    match std::env::var_os("FAE_HOST") {
        Some(home) if !home.is_empty() => expand_home(PathBuf::from(home)),
        _ => dirs::home_dir()
            .map(|home| home.join(".fae"))
            .unwrap_or_else(|| PathBuf::from(".fae")),
    }
}

fn expand_home(path: PathBuf) -> PathBuf {
    let Some(path_str) = path.to_str() else {
        return path;
    };
    if path_str == "~" {
        return dirs::home_dir().unwrap_or(path);
    }
    if let Some(rest) = path_str.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path
}

#[async_trait::async_trait]
impl WorkflowMetadataLoader for WorkflowMetadata {
    async fn load(&self, _workflow_id: &str) -> anyhow::Result<WorkflowMetadata> {
        Ok(self.clone())
    }
}

#[derive(Clone)]
pub struct WorkflowPlanBuilder {
    metadata_loader: Arc<dyn WorkflowMetadataLoader>,
    hooks: Vec<Arc<dyn WorkflowHookBuilder>>,
}

impl std::fmt::Debug for WorkflowPlanBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowPlanBuilder")
            .field("metadata_loader", &self.metadata_loader)
            .field("hook_count", &self.hooks.len())
            .finish()
    }
}

impl WorkflowPlanBuilder {
    pub fn new(loader: impl WorkflowMetadataLoader) -> Self {
        Self {
            metadata_loader: Arc::new(loader),
            hooks: Vec::new(),
        }
    }

    pub fn hooks(&self) -> &[Arc<dyn WorkflowHookBuilder>] {
        &self.hooks
    }

    pub fn add_hook<H>(&mut self, builder: H)
    where
        H: WorkflowHookBuilder,
    {
        self.hooks.push(Arc::new(builder));
    }

    pub fn add_hook_arc(&mut self, builder: Arc<dyn WorkflowHookBuilder>) {
        self.hooks.push(builder);
    }

    pub fn remove_hooks(&mut self) -> Vec<Arc<dyn WorkflowHookBuilder>> {
        std::mem::take(&mut self.hooks)
    }

    async fn build_hook(&self) -> Arc<dyn WorkflowHook> {
        let mut hooks = Vec::with_capacity(self.hooks.len());
        for builder in &self.hooks {
            hooks.push(builder.build().await);
        }
        Arc::new(CompositeWorkflowHook::new(hooks))
    }
}

#[async_trait::async_trait]
impl PlanBuilderWithEnv<WorkflowEnv> for WorkflowPlanBuilder {
    async fn build(
        &self,
        _rt: crate::RT,
        ctx: Ctx,
        env: WorkflowEnv,
    ) -> anyhow::Result<Box<dyn Plan>> {
        let metadata = self.metadata_loader.load(&env.workflow_id).await?;
        let hook = self.build_hook().await;
        build_workflow_plan(metadata, ctx, env, hook)
    }
}

fn build_workflow_plan(
    metadata: WorkflowMetadata,
    ctx: Ctx,
    env: WorkflowEnv,
    hook: Arc<dyn WorkflowHook>,
) -> anyhow::Result<Box<dyn Plan>> {
    anyhow::ensure!(!env.user_id.trim().is_empty(), "user_id cannot be empty");
    let complete_context = env.completes_context();
    let WorkflowEnv {
        workflow_id: _,
        input,
        user_id,
        session,
    } = env;
    session.set_user_id(user_id);
    metadata.validate()?;
    let current = metadata
        .nodes
        .iter()
        .find_map(|(id, node)| {
            matches!(
                node,
                WorkflowNode::Start { .. } | WorkflowNode::ParallelStart { .. }
            )
            .then_some(id.clone())
        })
        .ok_or_else(|| anyhow::anyhow!("workflow has no start node"))?;

    if requires_dag_execution(&metadata) {
        return Ok(Box::new(DagWorkflowPlan::new(
            metadata,
            input,
            session,
            ctx,
            current,
            complete_context,
            hook,
        )));
    }

    Ok(Box::new(WorkflowPlan {
        id: format!("workflow-{}-{}", metadata.id, wd_tools::uuid::v4()),
        metadata,
        input,
        session,
        ctx,
        current,
        outputs: HashMap::new(),
        loops: HashMap::new(),
        last_output: None,
        pending: None,
        task_sequence: 0,
        finished: false,
        complete_context,
        hook,
    }))
}

#[derive(Debug)]
enum PendingAction {
    Workflow,
    Tool,
    Session,
    SingleAgent(CommonSession),
    Extension,
}

#[derive(Debug)]
struct WorkflowPlan {
    id: String,
    metadata: WorkflowMetadata,
    input: Value,
    session: CommonSession,
    ctx: Ctx,
    current: String,
    outputs: HashMap<String, Value>,
    loops: HashMap<String, usize>,
    last_output: Option<Value>,
    pending: Option<PendingAction>,
    task_sequence: usize,
    finished: bool,
    complete_context: bool,
    hook: Arc<dyn WorkflowHook>,
}

#[derive(Debug)]
struct DagPendingAction {
    node_id: String,
    action: PendingAction,
}

#[derive(Debug)]
struct DagWorkflowPlan {
    id: String,
    metadata: WorkflowMetadata,
    input: Value,
    session: CommonSession,
    ctx: Ctx,
    start: String,
    predecessors: HashMap<String, HashSet<String>>,
    edge_states: HashMap<String, HashMap<String, bool>>,
    ready: VecDeque<String>,
    scheduled: HashSet<String>,
    skipped: HashSet<String>,
    outputs: HashMap<String, Value>,
    loops: HashMap<String, usize>,
    pending: HashMap<String, DagPendingAction>,
    task_sequence: usize,
    finished: bool,
    complete_context: bool,
    hook: Arc<dyn WorkflowHook>,
    active_node: Option<String>,
}

async fn invoke_workflow_hook(
    hook: &dyn WorkflowHook,
    context: &WorkflowHookContext<'_>,
) -> anyhow::Result<()> {
    match context.node {
        WorkflowNode::Start { .. } => hook.on_start(context).await,
        WorkflowNode::ParallelStart { .. } => hook.on_parallel_start(context).await,
        WorkflowNode::Execute { .. } => hook.on_execute(context).await,
        WorkflowNode::Decision { .. } => hook.on_decision(context).await,
        WorkflowNode::Loop { .. } => hook.on_loop(context).await,
        WorkflowNode::End { .. } => hook.on_end(context).await,
        WorkflowNode::JoinEnd { .. } => hook.on_join_end(context).await,
    }
}

impl WorkflowPlan {
    async fn invoke_hook(
        &self,
        node_id: &str,
        node: &WorkflowNode,
        phase: WorkflowHookPhase<'_>,
    ) -> anyhow::Result<()> {
        invoke_workflow_hook(
            self.hook.as_ref(),
            &WorkflowHookContext {
                ctx: &self.ctx,
                plan_id: &self.id,
                workflow_id: &self.metadata.id,
                node_id,
                node,
                phase,
            },
        )
        .await
    }

    fn emit(&self, node_id: impl Into<String>, data: SessionEventData) -> anyhow::Result<()> {
        self.session.emit(SessionEvent::workflow(
            self.metadata.id.clone(),
            node_id,
            data,
        ))
    }

    fn emit_node_completed(
        &self,
        node_id: impl Into<String>,
        output: Value,
        finished: bool,
    ) -> anyhow::Result<()> {
        self.emit(
            node_id,
            SessionEventData::NodeCompleted { output, finished },
        )
    }

    fn values(&self) -> WorkflowValues<'_> {
        WorkflowValues {
            input: &self.input,
            outputs: &self.outputs,
            loops: &self.loops,
            last_output: self.last_output.as_ref(),
        }
    }

    async fn advance(&mut self) -> anyhow::Result<PlanNext> {
        loop {
            let node_id = self.current.clone();
            let node = self.metadata.nodes.get(&node_id).cloned().ok_or_else(|| {
                anyhow::anyhow!("workflow node `{}` does not exist", self.current)
            })?;
            if matches!(
                node,
                WorkflowNode::ParallelStart { .. } | WorkflowNode::JoinEnd { .. }
            ) {
                anyhow::bail!("parallel workflow node reached by the sequential executor");
            }
            self.invoke_hook(&node_id, &node, WorkflowHookPhase::Before)
                .await?;
            let hook_node = node.clone();

            match node {
                WorkflowNode::ParallelStart { .. } | WorkflowNode::JoinEnd { .. } => unreachable!(),
                WorkflowNode::Start { next } => {
                    self.emit_node_completed(self.current.clone(), self.input.clone(), false)?;
                    self.invoke_hook(
                        &node_id,
                        &hook_node,
                        WorkflowHookPhase::After {
                            output: Some(&self.input),
                        },
                    )
                    .await?;
                    self.current = only_target(&self.current, "next", &next)?;
                }
                WorkflowNode::End { output } => {
                    let output = match output {
                        Some(template) => self.values().resolve(&template)?,
                        None => self
                            .last_output
                            .clone()
                            .unwrap_or_else(|| self.input.clone()),
                    };
                    self.emit_node_completed(self.current.clone(), output.clone(), true)?;
                    self.invoke_hook(
                        &node_id,
                        &hook_node,
                        WorkflowHookPhase::After {
                            output: Some(&output),
                        },
                    )
                    .await?;
                    if self.complete_context {
                        self.ctx.over(Box::new(output));
                    }
                    self.finished = true;
                    return Ok(PlanNext::End);
                }
                WorkflowNode::Decision {
                    condition,
                    on_true,
                    on_false,
                } => {
                    let result = self.values().evaluate(&condition)?;
                    let output = Value::Bool(result);
                    self.emit_node_completed(self.current.clone(), output.clone(), false)?;
                    self.invoke_hook(
                        &node_id,
                        &hook_node,
                        WorkflowHookPhase::After {
                            output: Some(&output),
                        },
                    )
                    .await?;
                    let selected = if result { &on_true } else { &on_false };
                    self.current = only_target(
                        &self.current,
                        if result { "on_true" } else { "on_false" },
                        selected,
                    )?;
                }
                WorkflowNode::Loop {
                    condition,
                    body,
                    next,
                    max_iterations,
                } => {
                    let continues = self.values().evaluate(&condition)?;
                    let output = if continues {
                        let iteration = self.loops.entry(self.current.clone()).or_default();
                        anyhow::ensure!(
                            *iteration < max_iterations,
                            "loop node `{}` exceeded max_iterations ({max_iterations})",
                            self.current
                        );
                        *iteration += 1;
                        let iteration = *iteration;
                        let output = json!({
                            "continues": true,
                            "iteration": iteration,
                        });
                        self.current = body;
                        output
                    } else {
                        let output = json!({
                            "continues": false,
                            "iteration": self
                                .loops
                                .get(&self.current)
                                .copied()
                                .unwrap_or_default(),
                        });
                        self.current = next;
                        output
                    };
                    self.emit_node_completed(node_id.clone(), output.clone(), false)?;
                    self.invoke_hook(
                        &node_id,
                        &hook_node,
                        WorkflowHookPhase::After {
                            output: Some(&output),
                        },
                    )
                    .await?;
                }
                WorkflowNode::Execute { action, .. } => {
                    let task = self.action_task(action).await?;
                    return Ok(PlanNext::Tasks(vec![task]));
                }
            }
        }
    }

    async fn action_task(&mut self, action: WorkflowAction) -> anyhow::Result<TaskRequest> {
        anyhow::ensure!(
            self.pending.is_none(),
            "workflow already has a pending action"
        );
        self.task_sequence += 1;
        let meta = TaskMeta {
            id: format!(
                "{}-{}-{}",
                self.metadata.id, self.current, self.task_sequence
            ),
            ..Default::default()
        };

        let task = match action {
            WorkflowAction::Workflow { workflow_id, input } => {
                let input = self.values().resolve(&input)?;
                let (env, _) =
                    WorkflowEnv::new_with_user_id(workflow_id, input, self.session.user_id());
                self.pending = Some(PendingAction::Workflow);
                TaskReq {
                    ctx: self.ctx.clone(),
                    meta: TaskMeta {
                        ty: TaskType::Workflow,
                        ..meta
                    },
                    req: env,
                }
                .into_request()
            }
            WorkflowAction::Tool {
                tool_name,
                arguments,
            } => {
                let arguments = self.values().resolve(&arguments)?;
                self.pending = Some(PendingAction::Tool);
                TaskReq {
                    ctx: self.ctx.clone(),
                    meta: TaskMeta {
                        ty: TaskType::Tool,
                        ..meta
                    },
                    req: ToolRequest::new(tool_name, serde_json::to_string(&arguments)?),
                }
                .into_request()
            }
            WorkflowAction::Session { request } => {
                let request = resolve_serializable(&self.values(), &request)?;
                self.pending = Some(PendingAction::Session);
                TaskReq {
                    ctx: self.ctx.clone(),
                    meta: TaskMeta {
                        ty: TaskType::Session,
                        ..meta
                    },
                    req: request,
                }
                .into_request()
            }
            WorkflowAction::SingleAgent { source, input } => {
                let input = resolved_string(&self.values().resolve(&input)?)?;
                let (env, session) = SingleAgentEnv::new_with_session(
                    source,
                    input,
                    self.session.clone(),
                    self.metadata.id.clone(),
                    self.current.clone(),
                );
                let child = self
                    .ctx
                    .get_engine()
                    .call(
                        self.ctx.clone(),
                        to_plan_ty::<SingleAgentEnv>(),
                        Box::new(env),
                    )
                    .await?;
                self.pending = Some(PendingAction::SingleAgent(session));
                TaskReq {
                    ctx: self.ctx.clone(),
                    meta: TaskMeta {
                        ty: TaskType::Plan,
                        ..meta
                    },
                    req: child,
                }
                .into_request()
            }
            WorkflowAction::Python {
                code,
                arguments,
                task_type,
            } => {
                anyhow::ensure!(
                    !task_type.trim().is_empty(),
                    "python action task_type cannot be empty"
                );
                let request = WorkflowActionRequest {
                    action: "python".to_string(),
                    payload: json!({
                        "code": code,
                        "arguments": self.values().resolve(&arguments)?,
                    }),
                };
                self.pending = Some(PendingAction::Extension);
                extension_task(self.ctx.clone(), meta, task_type, request)
            }
            WorkflowAction::Custom { task_type, request } => {
                anyhow::ensure!(
                    !task_type.trim().is_empty(),
                    "custom action task_type cannot be empty"
                );
                let request = WorkflowActionRequest {
                    action: "custom".to_string(),
                    payload: self.values().resolve(&request)?,
                };
                self.pending = Some(PendingAction::Extension);
                extension_task(self.ctx.clone(), meta, task_type, request)
            }
        };
        Ok(task)
    }

    async fn action_output(&mut self, mut response: TaskResponse) -> anyhow::Result<Value> {
        match self
            .pending
            .take()
            .ok_or_else(|| anyhow::anyhow!("workflow received an unexpected task response"))?
        {
            PendingAction::Workflow => {
                let response =
                    TaskResp::<Value>::try_from_response(&mut response).ok_or_else(|| {
                        anyhow::anyhow!("workflow expected a workflow Value response")
                    })?;
                Ok(response.resp)
            }
            PendingAction::Tool => {
                let mut response = TaskResp::<ToolResponse>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a ToolResponse"))?
                    .resp;
                loop {
                    match response.next().await? {
                        ToolRespItem::Streaming(_) => {}
                        ToolRespItem::Completed(output) => {
                            break Ok(serde_json::from_str(&output)
                                .unwrap_or_else(|_| Value::String(output)));
                        }
                    }
                }
            }
            PendingAction::Session => {
                let response = TaskResp::<SessionResponse>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a SessionResponse"))?;
                Ok(serde_json::to_value(response.resp)?)
            }
            PendingAction::SingleAgent(session) => {
                TaskResp::<()>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a child plan response"))?;
                loop {
                    let event = session.answer().await?.ok_or_else(|| {
                        anyhow::anyhow!("single-agent session ended without a final event")
                    })?;
                    match event.event_data()? {
                        SessionEventData::Completed { content } => {
                            break Ok(Value::String(content));
                        }
                        SessionEventData::Failed { error } => anyhow::bail!(error),
                        _ => {}
                    }
                }
            }
            PendingAction::Extension => {
                let response = TaskResp::<WorkflowActionResponse>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a WorkflowActionResponse"))?;
                Ok(response.resp.output)
            }
        }
    }
}

#[async_trait::async_trait]
impl Plan for WorkflowPlan {
    fn id(&self) -> &str {
        &self.id
    }

    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        self.advance().await
    }

    async fn next(&mut self, response: TaskResponse) -> anyhow::Result<PlanNext> {
        anyhow::ensure!(!self.finished, "workflow has already finished");
        let node_id = self.current.clone();
        let node = self
            .metadata
            .nodes
            .get(&node_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("workflow node `{node_id}` does not exist"))?;
        let next = match &node {
            WorkflowNode::Execute { next, .. } => only_target(&node_id, "next", next)?,
            _ => anyhow::bail!("workflow is not waiting at an execute node"),
        };
        let output = self.action_output(response).await?;
        self.emit_node_completed(node_id.clone(), output.clone(), false)?;
        self.invoke_hook(
            &node_id,
            &node,
            WorkflowHookPhase::After {
                output: Some(&output),
            },
        )
        .await?;
        self.outputs.insert(node_id, output.clone());
        self.last_output = Some(output);
        self.current = next;
        self.advance().await
    }

    async fn abort(&mut self, _code: i32, error: String) {
        self.pending = None;
        if let Some(node) = self.metadata.nodes.get(&self.current) {
            let _ = self
                .invoke_hook(
                    &self.current,
                    node,
                    WorkflowHookPhase::Failed { error: &error },
                )
                .await;
        }
        let _ = self.emit(self.current.clone(), SessionEventData::Failed { error });
    }
}

impl DagWorkflowPlan {
    fn new(
        metadata: WorkflowMetadata,
        input: Value,
        session: CommonSession,
        ctx: Ctx,
        start: String,
        complete_context: bool,
        hook: Arc<dyn WorkflowHook>,
    ) -> Self {
        let mut predecessors = HashMap::<String, HashSet<String>>::new();
        for (source, node) in &metadata.nodes {
            for target in node.successors() {
                predecessors
                    .entry(target.to_string())
                    .or_default()
                    .insert(source.clone());
            }
        }

        Self {
            id: format!("workflow-{}-{}", metadata.id, wd_tools::uuid::v4()),
            metadata,
            input,
            session,
            ctx,
            start: start.clone(),
            predecessors,
            edge_states: HashMap::new(),
            ready: VecDeque::from([start]),
            scheduled: HashSet::new(),
            skipped: HashSet::new(),
            outputs: HashMap::new(),
            loops: HashMap::new(),
            pending: HashMap::new(),
            task_sequence: 0,
            finished: false,
            complete_context,
            hook,
            active_node: None,
        }
    }

    async fn invoke_hook(
        &self,
        node_id: &str,
        node: &WorkflowNode,
        phase: WorkflowHookPhase<'_>,
    ) -> anyhow::Result<()> {
        invoke_workflow_hook(
            self.hook.as_ref(),
            &WorkflowHookContext {
                ctx: &self.ctx,
                plan_id: &self.id,
                workflow_id: &self.metadata.id,
                node_id,
                node,
                phase,
            },
        )
        .await
    }

    fn emit(&self, node_id: impl Into<String>, data: SessionEventData) -> anyhow::Result<()> {
        self.session.emit(SessionEvent::workflow(
            self.metadata.id.clone(),
            node_id,
            data,
        ))
    }

    fn emit_node_completed(
        &self,
        node_id: impl Into<String>,
        output: Value,
        finished: bool,
    ) -> anyhow::Result<()> {
        self.emit(
            node_id,
            SessionEventData::NodeCompleted { output, finished },
        )
    }

    fn values(&self) -> WorkflowValues<'_> {
        WorkflowValues {
            input: &self.input,
            outputs: &self.outputs,
            loops: &self.loops,
            last_output: None,
        }
    }

    async fn action_task(
        &mut self,
        node_id: &str,
        action: WorkflowAction,
    ) -> anyhow::Result<TaskRequest> {
        self.task_sequence += 1;
        let meta = TaskMeta {
            id: format!("{}-{}-{}", self.metadata.id, node_id, self.task_sequence),
            ..Default::default()
        };

        let (task, pending) = match action {
            WorkflowAction::Workflow { workflow_id, input } => {
                let input = self.values().resolve(&input)?;
                let (env, _) =
                    WorkflowEnv::new_with_user_id(workflow_id, input, self.session.user_id());
                (
                    TaskReq {
                        ctx: self.ctx.clone(),
                        meta: TaskMeta {
                            ty: TaskType::Workflow,
                            ..meta
                        },
                        req: env,
                    }
                    .into_request(),
                    PendingAction::Workflow,
                )
            }
            WorkflowAction::Tool {
                tool_name,
                arguments,
            } => {
                let arguments = self.values().resolve(&arguments)?;
                (
                    TaskReq {
                        ctx: self.ctx.clone(),
                        meta: TaskMeta {
                            ty: TaskType::Tool,
                            ..meta
                        },
                        req: ToolRequest::new(tool_name, serde_json::to_string(&arguments)?),
                    }
                    .into_request(),
                    PendingAction::Tool,
                )
            }
            WorkflowAction::Session { request } => {
                let request = resolve_serializable(&self.values(), &request)?;
                (
                    TaskReq {
                        ctx: self.ctx.clone(),
                        meta: TaskMeta {
                            ty: TaskType::Session,
                            ..meta
                        },
                        req: request,
                    }
                    .into_request(),
                    PendingAction::Session,
                )
            }
            WorkflowAction::SingleAgent { source, input } => {
                let input = resolved_string(&self.values().resolve(&input)?)?;
                let (env, session) = SingleAgentEnv::new_with_session(
                    source,
                    input,
                    self.session.clone(),
                    self.metadata.id.clone(),
                    node_id.to_string(),
                );
                let child = self
                    .ctx
                    .get_engine()
                    .call(
                        self.ctx.clone(),
                        to_plan_ty::<SingleAgentEnv>(),
                        Box::new(env),
                    )
                    .await?;
                (
                    TaskReq {
                        ctx: self.ctx.clone(),
                        meta: TaskMeta {
                            ty: TaskType::Plan,
                            ..meta
                        },
                        req: child,
                    }
                    .into_request(),
                    PendingAction::SingleAgent(session),
                )
            }
            WorkflowAction::Python {
                code,
                arguments,
                task_type,
            } => {
                anyhow::ensure!(
                    !task_type.trim().is_empty(),
                    "python action task_type cannot be empty"
                );
                let request = WorkflowActionRequest {
                    action: "python".to_string(),
                    payload: json!({
                        "code": code,
                        "arguments": self.values().resolve(&arguments)?,
                    }),
                };
                (
                    extension_task(self.ctx.clone(), meta, task_type, request),
                    PendingAction::Extension,
                )
            }
            WorkflowAction::Custom { task_type, request } => {
                anyhow::ensure!(
                    !task_type.trim().is_empty(),
                    "custom action task_type cannot be empty"
                );
                let request = WorkflowActionRequest {
                    action: "custom".to_string(),
                    payload: self.values().resolve(&request)?,
                };
                (
                    extension_task(self.ctx.clone(), meta, task_type, request),
                    PendingAction::Extension,
                )
            }
        };
        self.pending.insert(
            task.meta.id.clone(),
            DagPendingAction {
                node_id: node_id.to_string(),
                action: pending,
            },
        );
        Ok(task)
    }

    fn resolve_outgoing(&mut self, source: &str, active_targets: &[String]) -> anyhow::Result<()> {
        let node = self
            .metadata
            .nodes
            .get(source)
            .ok_or_else(|| anyhow::anyhow!("workflow node `{source}` does not exist"))?;
        let mut seen = HashSet::new();
        let possible = node
            .successors()
            .into_iter()
            .filter(|target| seen.insert(*target))
            .map(str::to_string)
            .collect::<Vec<_>>();
        let possible_set = possible.iter().cloned().collect::<HashSet<_>>();
        let active = active_targets.iter().cloned().collect::<HashSet<_>>();
        anyhow::ensure!(
            active.is_subset(&possible_set),
            "node `{source}` activated a target outside its declared successors"
        );

        let mut resolved = possible
            .into_iter()
            .map(|target| (source.to_string(), target.clone(), active.contains(&target)))
            .collect::<VecDeque<_>>();

        while let Some((source, target, is_active)) = resolved.pop_front() {
            let states = self.edge_states.entry(target.clone()).or_default();
            anyhow::ensure!(
                states.insert(source.clone(), is_active).is_none(),
                "workflow edge `{source}` -> `{target}` was resolved more than once"
            );
            let predecessors = self
                .predecessors
                .get(&target)
                .ok_or_else(|| anyhow::anyhow!("node `{target}` has no predecessors"))?;
            if states.len() != predecessors.len() {
                continue;
            }

            if states.values().any(|active| *active) {
                if !self.scheduled.contains(&target) {
                    self.ready.push_back(target);
                }
                continue;
            }

            if !self.skipped.insert(target.clone()) {
                continue;
            }
            let skipped = self
                .metadata
                .nodes
                .get(&target)
                .ok_or_else(|| anyhow::anyhow!("workflow node `{target}` does not exist"))?;
            let mut seen = HashSet::new();
            for successor in skipped
                .successors()
                .into_iter()
                .filter(|successor| seen.insert(*successor))
            {
                resolved.push_back((target.clone(), successor.to_string(), false));
            }
        }
        Ok(())
    }

    fn end_output(&self, node_id: &str, output: Option<Value>) -> anyhow::Result<Value> {
        if let Some(template) = output {
            return self.values().resolve(&template);
        }

        let active_outputs = self
            .edge_states
            .get(node_id)
            .into_iter()
            .flat_map(|states| states.iter())
            .filter(|(_, active)| **active)
            .filter_map(|(source, _)| {
                self.outputs
                    .get(source)
                    .cloned()
                    .map(|output| (source.clone(), output))
            })
            .collect::<Vec<_>>();
        match active_outputs.as_slice() {
            [] => Ok(self.input.clone()),
            [(_, output)] => Ok(output.clone()),
            _ => Ok(Value::Object(active_outputs.into_iter().collect())),
        }
    }

    async fn advance(&mut self) -> anyhow::Result<PlanNext> {
        let mut tasks = Vec::new();

        while let Some(node_id) = self.ready.pop_front() {
            if !self.scheduled.insert(node_id.clone()) {
                continue;
            }
            let node = self
                .metadata
                .nodes
                .get(&node_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("workflow node `{node_id}` does not exist"))?;
            self.active_node = Some(node_id.clone());
            self.invoke_hook(&node_id, &node, WorkflowHookPhase::Before)
                .await?;
            let hook_node = node.clone();

            match node {
                WorkflowNode::Start { next } | WorkflowNode::ParallelStart { next } => {
                    self.emit_node_completed(node_id.clone(), self.input.clone(), false)?;
                    self.invoke_hook(
                        &node_id,
                        &hook_node,
                        WorkflowHookPhase::After {
                            output: Some(&self.input),
                        },
                    )
                    .await?;
                    self.resolve_outgoing(&node_id, &next)?;
                }
                WorkflowNode::Execute { action, .. } => {
                    tasks.push(self.action_task(&node_id, action).await?);
                }
                WorkflowNode::Decision {
                    condition,
                    on_true,
                    on_false,
                } => {
                    let result = self.values().evaluate(&condition)?;
                    let output = Value::Bool(result);
                    self.emit_node_completed(node_id.clone(), output.clone(), false)?;
                    self.invoke_hook(
                        &node_id,
                        &hook_node,
                        WorkflowHookPhase::After {
                            output: Some(&output),
                        },
                    )
                    .await?;
                    self.resolve_outgoing(&node_id, if result { &on_true } else { &on_false })?;
                }
                WorkflowNode::End { output } | WorkflowNode::JoinEnd { output } => {
                    anyhow::ensure!(
                        tasks.is_empty() && self.pending.is_empty(),
                        "workflow end became ready while actions were still pending"
                    );
                    let output = self.end_output(&node_id, output)?;
                    self.emit_node_completed(node_id.clone(), output.clone(), true)?;
                    self.invoke_hook(
                        &node_id,
                        &hook_node,
                        WorkflowHookPhase::After {
                            output: Some(&output),
                        },
                    )
                    .await?;
                    if self.complete_context {
                        self.ctx.over(Box::new(output));
                    }
                    self.active_node = None;
                    self.finished = true;
                    return Ok(PlanNext::End);
                }
                WorkflowNode::Loop { .. } => {
                    anyhow::bail!("loop node reached by the DAG workflow executor")
                }
            }
            self.active_node = None;
        }

        if !tasks.is_empty() {
            return Ok(PlanNext::Tasks(tasks));
        }
        anyhow::bail!("DAG workflow stalled before reaching its end node")
    }
}

#[async_trait::async_trait]
impl Plan for DagWorkflowPlan {
    fn id(&self) -> &str {
        &self.id
    }

    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        self.advance().await
    }

    async fn next(&mut self, mut response: TaskResponse) -> anyhow::Result<PlanNext> {
        anyhow::ensure!(!self.finished, "workflow has already finished");
        let task_id = response.meta.id.clone();
        let pending = self
            .pending
            .remove(&task_id)
            .ok_or_else(|| anyhow::anyhow!("workflow received unexpected task `{task_id}`"))?;
        self.active_node = Some(pending.node_id.clone());
        let output = match pending.action {
            PendingAction::Workflow => {
                let response =
                    TaskResp::<Value>::try_from_response(&mut response).ok_or_else(|| {
                        anyhow::anyhow!("workflow expected a workflow Value response")
                    })?;
                response.resp
            }
            PendingAction::Tool => {
                let mut response = TaskResp::<ToolResponse>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a ToolResponse"))?
                    .resp;
                loop {
                    match response.next().await? {
                        ToolRespItem::Streaming(_) => {}
                        ToolRespItem::Completed(output) => {
                            break serde_json::from_str(&output)
                                .unwrap_or_else(|_| Value::String(output));
                        }
                    }
                }
            }
            PendingAction::Session => {
                let response = TaskResp::<SessionResponse>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a SessionResponse"))?;
                serde_json::to_value(response.resp)?
            }
            PendingAction::SingleAgent(session) => {
                TaskResp::<()>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a child plan response"))?;
                loop {
                    let event = session.answer().await?.ok_or_else(|| {
                        anyhow::anyhow!("single-agent session ended without a final event")
                    })?;
                    match event.event_data()? {
                        SessionEventData::Completed { content } => {
                            break Value::String(content);
                        }
                        SessionEventData::Failed { error } => anyhow::bail!(error),
                        _ => {}
                    }
                }
            }
            PendingAction::Extension => {
                let response = TaskResp::<WorkflowActionResponse>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a WorkflowActionResponse"))?;
                response.resp.output
            }
        };

        let node = self
            .metadata
            .nodes
            .get(&pending.node_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("workflow node `{}` does not exist", pending.node_id))?;
        let next = match &node {
            WorkflowNode::Execute { next, .. } => next.clone(),
            _ => anyhow::bail!(
                "workflow is not waiting at execute node `{}`",
                pending.node_id
            ),
        };
        self.emit_node_completed(pending.node_id.clone(), output.clone(), false)?;
        self.invoke_hook(
            &pending.node_id,
            &node,
            WorkflowHookPhase::After {
                output: Some(&output),
            },
        )
        .await?;
        self.outputs.insert(pending.node_id.clone(), output);
        self.resolve_outgoing(&pending.node_id, &next)?;
        self.active_node = None;

        if self.pending.is_empty() {
            self.advance().await
        } else {
            Ok(PlanNext::Tasks(Vec::new()))
        }
    }

    async fn abort(&mut self, _code: i32, error: String) {
        let mut node_ids = match self.active_node.take() {
            Some(node_id) => vec![node_id],
            None => self
                .pending
                .values()
                .map(|pending| pending.node_id.clone())
                .collect(),
        };
        node_ids.sort();
        node_ids.dedup();
        if node_ids.is_empty() {
            node_ids.push(self.start.clone());
        }
        for node_id in node_ids {
            if let Some(node) = self.metadata.nodes.get(&node_id) {
                let _ = self
                    .invoke_hook(&node_id, node, WorkflowHookPhase::Failed { error: &error })
                    .await;
            }
        }
        self.pending.clear();
        let _ = self.emit(self.start.clone(), SessionEventData::Failed { error });
    }
}

fn extension_task(
    ctx: Ctx,
    meta: TaskMeta,
    task_type: String,
    request: WorkflowActionRequest,
) -> TaskRequest {
    TaskReq {
        ctx,
        meta: TaskMeta {
            ty: TaskType::Any(task_type),
            ..meta
        },
        req: request,
    }
    .into_request()
}

fn resolve_serializable<T>(values: &WorkflowValues<'_>, input: &T) -> anyhow::Result<T>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    Ok(serde_json::from_value(
        values.resolve(&serde_json::to_value(input)?)?,
    )?)
}

fn resolved_string(value: &Value) -> anyhow::Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Null => Ok(String::new()),
        Value::Bool(_) | Value::Number(_) => Ok(value.to_string()),
        Value::Array(_) | Value::Object(_) => Ok(serde_json::to_string(value)?),
    }
}

fn only_target(node_id: &str, field: &str, targets: &[String]) -> anyhow::Result<String> {
    let [target] = targets else {
        anyhow::bail!(
            "sequential workflow node `{node_id}` field `{field}` must contain exactly one target"
        );
    };
    Ok(target.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContextNull, WorkflowCondition, WorkflowMetadataBuilder};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct RecordingWorkflowHookBuilder {
        name: &'static str,
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl WorkflowHookBuilder for RecordingWorkflowHookBuilder {
        async fn build(&self) -> Arc<dyn WorkflowHook> {
            self.events
                .lock()
                .unwrap()
                .push(format!("build:{}", self.name));
            Arc::new(RecordingWorkflowHook {
                name: self.name,
                events: self.events.clone(),
            })
        }
    }

    #[derive(Debug)]
    struct RecordingWorkflowHook {
        name: &'static str,
        events: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingWorkflowHook {
        fn phase(ctx: &WorkflowHookContext<'_>) -> String {
            match ctx.phase {
                WorkflowHookPhase::Before => "before".to_string(),
                WorkflowHookPhase::After { output } => format!(
                    "after:{}",
                    output
                        .map(Value::to_string)
                        .unwrap_or_else(|| "none".to_string())
                ),
                WorkflowHookPhase::Failed { error } => format!("failed:{error}"),
            }
        }

        fn record(&self, ctx: &WorkflowHookContext<'_>) {
            self.events.lock().unwrap().push(format!(
                "{}:{}:{}",
                self.name,
                ctx.node_id,
                Self::phase(ctx)
            ));
        }

        fn record_callback(&self, callback: &str, ctx: &WorkflowHookContext<'_>) {
            self.events.lock().unwrap().push(format!(
                "{}:{callback}:{}:{}",
                self.name,
                ctx.node_id,
                Self::phase(ctx)
            ));
        }
    }

    #[async_trait::async_trait]
    impl WorkflowHook for RecordingWorkflowHook {
        async fn on_start(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
            self.record(ctx);
            Ok(())
        }

        async fn on_parallel_start(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
            self.record_callback("parallel_start", ctx);
            Ok(())
        }

        async fn on_execute(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
            self.record(ctx);
            Ok(())
        }

        async fn on_decision(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
            self.record(ctx);
            Ok(())
        }

        async fn on_loop(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
            self.record(ctx);
            Ok(())
        }

        async fn on_end(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
            self.record(ctx);
            Ok(())
        }

        async fn on_join_end(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
            self.record_callback("join_end", ctx);
            Ok(())
        }
    }

    fn recording_builder(
        metadata: WorkflowMetadata,
        events: Arc<Mutex<Vec<String>>>,
    ) -> WorkflowPlanBuilder {
        let mut builder = WorkflowPlanBuilder::new(metadata);
        builder.add_hook(RecordingWorkflowHookBuilder {
            name: "first",
            events: events.clone(),
        });
        builder.add_hook(RecordingWorkflowHookBuilder {
            name: "second",
            events,
        });
        builder
    }

    #[tokio::test]
    async fn workflow_builder_composes_hooks_in_registration_order() {
        let mut metadata = WorkflowMetadataBuilder::new("hooked", "Test workflow hooks");
        metadata.start("start", "execute").unwrap();
        metadata
            .execute(
                "execute",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: Value::Null,
                },
                "end",
            )
            .unwrap();
        metadata.end("end", Some(json!("done"))).unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let builder = recording_builder(metadata.build().unwrap(), events.clone());
        let (env, _) = WorkflowEnv::new("hooked", json!({"input": true}));
        let ctx = Ctx::new(Arc::new(ContextNull));
        let mut plan = builder
            .build(crate::RT::null(), ctx.clone(), env)
            .await
            .unwrap();

        let PlanNext::Tasks(mut tasks) = plan.init().await.unwrap() else {
            panic!("expected execute task");
        };
        let task = TaskReq::<WorkflowActionRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx,
                    meta: task.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"value": 1}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::End
        ));

        assert_eq!(
            *events.lock().unwrap(),
            [
                "build:first",
                "build:second",
                "first:start:before",
                "second:start:before",
                "first:start:after:{\"input\":true}",
                "second:start:after:{\"input\":true}",
                "first:execute:before",
                "second:execute:before",
                "first:execute:after:{\"value\":1}",
                "second:execute:after:{\"value\":1}",
                "first:end:before",
                "second:end:before",
                "first:end:after:\"done\"",
                "second:end:after:\"done\"",
            ]
        );
        assert_eq!(builder.hooks().len(), 2);
    }

    #[tokio::test]
    async fn workflow_hook_reports_failed_execute_node() {
        let mut metadata = WorkflowMetadataBuilder::new("hook-failure", "Test failed hook phase");
        metadata.start("start", "execute").unwrap();
        metadata
            .execute(
                "execute",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: Value::Null,
                },
                "end",
            )
            .unwrap();
        metadata.end("end", None).unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let builder = recording_builder(metadata.build().unwrap(), events.clone());
        let (env, _) = WorkflowEnv::new("hook-failure", Value::Null);
        let mut plan = builder
            .build(crate::RT::null(), Ctx::new(Arc::new(ContextNull)), env)
            .await
            .unwrap();

        assert!(matches!(plan.init().await.unwrap(), PlanNext::Tasks(_)));
        plan.abort(-1, "task failed".to_string()).await;

        let events = events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|event| event == "first:execute:failed:task failed")
        );
        assert!(
            events
                .iter()
                .any(|event| event == "second:execute:failed:task failed")
        );
    }

    #[tokio::test]
    async fn dag_workflow_routes_parallel_and_join_hooks() {
        let mut metadata = WorkflowMetadataBuilder::new("hooked-dag", "Test DAG workflow hooks");
        metadata
            .add_node(
                "start",
                WorkflowNode::ParallelStart {
                    next: vec!["left".to_string(), "right".to_string()],
                },
            )
            .unwrap();
        for node_id in ["left", "right"] {
            metadata
                .execute(
                    node_id,
                    WorkflowAction::Custom {
                        task_type: "fixture".to_string(),
                        request: json!({"node": node_id}),
                    },
                    "end",
                )
                .unwrap();
        }
        metadata
            .add_node("end", WorkflowNode::JoinEnd { output: None })
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let builder = recording_builder(metadata.build().unwrap(), events.clone());
        let (env, _) = WorkflowEnv::new("hooked-dag", Value::Null);
        let ctx = Ctx::new(Arc::new(ContextNull));
        let mut plan = builder
            .build(crate::RT::null(), ctx.clone(), env)
            .await
            .unwrap();

        let PlanNext::Tasks(mut tasks) = plan.init().await.unwrap() else {
            panic!("expected parallel execute tasks");
        };
        assert_eq!(tasks.len(), 2);
        let left = TaskReq::<WorkflowActionRequest>::try_from_request(&mut tasks[0]).unwrap();
        let right = TaskReq::<WorkflowActionRequest>::try_from_request(&mut tasks[1]).unwrap();

        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: left.meta,
                    resp: WorkflowActionResponse {
                        output: json!("left"),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::Tasks(tasks) if tasks.is_empty()
        ));
        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx,
                    meta: right.meta,
                    resp: WorkflowActionResponse {
                        output: json!("right"),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::End
        ));

        let events = events.lock().unwrap();
        for expected in [
            "first:parallel_start:start:before",
            "second:parallel_start:start:after:null",
            "first:join_end:end:before",
            "second:join_end:end:after:{\"left\":\"left\",\"right\":\"right\"}",
        ] {
            assert!(
                events.iter().any(|event| event == expected),
                "missing hook event `{expected}` in {events:?}"
            );
        }
    }

    #[tokio::test]
    async fn fae_loader_supports_runtime_add_replace_and_remove() {
        let loader = FAEWorkflowMetadataLoader::new();
        let plan_builder = WorkflowPlanBuilder::new(loader.clone());

        for value in [1, 2] {
            let mut metadata =
                WorkflowMetadataBuilder::new("dynamic", "Load dynamic workflow metadata");
            metadata.start("start", "end").unwrap();
            metadata.end("end", Some(json!(value))).unwrap();
            let replaced = loader.add(metadata.build().unwrap()).unwrap();
            assert_eq!(replaced.is_some(), value == 2);

            let (env, session) = WorkflowEnv::new("dynamic", Value::Null);
            let mut plan = plan_builder
                .build(crate::RT::null(), Ctx::new(Arc::new(ContextNull)), env)
                .await
                .unwrap();

            assert!(matches!(plan.init().await.unwrap(), PlanNext::End));
            assert_eq!(session.result().await.unwrap(), json!(value));
        }

        assert!(loader.remove("dynamic").unwrap().is_some());
        let (env, _) = WorkflowEnv::new("dynamic", Value::Null);
        assert!(
            plan_builder
                .build(crate::RT::null(), Ctx::new(Arc::new(ContextNull)), env)
                .await
                .unwrap_err()
                .to_string()
                .contains("load workflow `dynamic`")
        );
    }

    #[tokio::test]
    async fn fae_loader_loads_workflow_from_home_directory() {
        let home_dir =
            std::env::temp_dir().join(format!("fae-workflow-loader-{}", wd_tools::uuid::v4()));
        let workflows_dir = home_dir.join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();

        let mut builder = WorkflowMetadataBuilder::new("from-disk", "Load a workflow from disk");
        builder.start("start", "end").unwrap();
        builder.end("end", Some(json!("disk"))).unwrap();
        builder
            .build()
            .unwrap()
            .save_json(workflows_dir.join("from-disk.json"))
            .await
            .unwrap();

        let loader = FAEWorkflowMetadataLoader::with_home_dir(&home_dir);
        let metadata = loader.load("from-disk").await.unwrap();
        assert_eq!(metadata.id, "from-disk");

        tokio::fs::remove_dir_all(home_dir).await.unwrap();
    }

    #[tokio::test]
    async fn fae_loader_prefers_registered_workflow_over_disk() {
        let home_dir =
            std::env::temp_dir().join(format!("fae-workflow-loader-{}", wd_tools::uuid::v4()));
        let workflows_dir = home_dir.join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();

        for (output, save_to_disk) in [("disk", true), ("registered", false)] {
            let mut builder =
                WorkflowMetadataBuilder::new("precedence", "Prefer registered workflow metadata");
            builder.start("start", "end").unwrap();
            builder.end("end", Some(json!(output))).unwrap();
            let metadata = builder.build().unwrap();
            if save_to_disk {
                metadata
                    .save_json(workflows_dir.join("precedence.json"))
                    .await
                    .unwrap();
            } else {
                let loader = FAEWorkflowMetadataLoader::with_home_dir(&home_dir);
                loader.add(metadata).unwrap();
                let loaded = loader.load("precedence").await.unwrap();
                let WorkflowNode::End { output } = &loaded.nodes["end"] else {
                    panic!("expected end node");
                };
                assert_eq!(output.as_ref(), Some(&json!("registered")));
            }
        }

        tokio::fs::remove_dir_all(home_dir).await.unwrap();
    }

    #[tokio::test]
    async fn executes_deserialized_metadata_and_resolves_earlier_output() {
        let mut builder = WorkflowMetadataBuilder::new("example", "Test workflow execution");
        builder.start("start", "A").unwrap();
        builder
            .execute(
                "A",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: json!({"name": "{$input.name}"}),
                },
                "B",
            )
            .unwrap();
        builder
            .execute(
                "B",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: json!({"count": "{$A.output_field.count}"}),
                },
                "end",
            )
            .unwrap();
        builder.end("end", Some(json!("{$B.done}"))).unwrap();
        let metadata = builder.build().unwrap();
        let metadata = metadata.to_string().parse::<WorkflowMetadata>().unwrap();
        let plan_builder = WorkflowPlanBuilder::new(metadata);
        let (env, session) = WorkflowEnv::new("example", json!({"name": "Ada"}));
        let ctx = Ctx::new(Arc::new(ContextNull));
        let mut plan = plan_builder
            .build(crate::RT::null(), ctx.clone(), env)
            .await
            .unwrap();

        let PlanNext::Tasks(mut tasks) = plan.init().await.unwrap() else {
            panic!("expected first action");
        };
        assert_eq!(
            session
                .answer()
                .await
                .unwrap()
                .unwrap()
                .event_data()
                .unwrap(),
            SessionEventData::NodeCompleted {
                output: json!({"name": "Ada"}),
                finished: false,
            }
        );
        let first = TaskReq::<WorkflowActionRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert_eq!(first.req.payload, json!({"name": "Ada"}));

        let PlanNext::Tasks(mut tasks) = plan
            .next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: first.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"output_field": {"count": 3}}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap()
        else {
            panic!("expected second action");
        };
        assert_eq!(
            session
                .answer()
                .await
                .unwrap()
                .unwrap()
                .event_data()
                .unwrap(),
            SessionEventData::NodeCompleted {
                output: json!({"output_field": {"count": 3}}),
                finished: false,
            }
        );
        let second = TaskReq::<WorkflowActionRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert_eq!(second.req.payload, json!({"count": 3}));

        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx,
                    meta: second.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"done": true}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::End
        ));
        assert_eq!(
            session
                .answer()
                .await
                .unwrap()
                .unwrap()
                .event_data()
                .unwrap(),
            SessionEventData::NodeCompleted {
                output: json!({"done": true}),
                finished: false,
            }
        );
        let completed = session.answer().await.unwrap().unwrap();
        assert_eq!(completed.node_id.as_deref(), Some("end"));
        assert!(completed.is_terminal());
    }

    #[tokio::test]
    async fn emits_completion_for_control_flow_nodes() {
        let mut builder =
            WorkflowMetadataBuilder::new("control-flow", "Test workflow control flow");
        builder.start("start", "decision").unwrap();
        builder
            .decision(
                "decision",
                WorkflowCondition::Truthy { value: json!(true) },
                "end",
                "end",
            )
            .unwrap();
        builder.end("end", Some(json!("done"))).unwrap();
        let plan_builder = WorkflowPlanBuilder::new(builder.build().unwrap());
        let (env, session) = WorkflowEnv::new("control-flow", Value::Null);
        let mut plan = plan_builder
            .build(crate::RT::null(), Ctx::new(Arc::new(ContextNull)), env)
            .await
            .unwrap();

        assert!(matches!(plan.init().await.unwrap(), PlanNext::End));
        for (node_id, output, finished) in [
            ("start", Value::Null, false),
            ("decision", Value::Bool(true), false),
            ("end", json!("done"), true),
        ] {
            let event = session.answer().await.unwrap().unwrap();
            assert_eq!(event.node_id.as_deref(), Some(node_id));
            assert_eq!(
                event.event_data().unwrap(),
                SessionEventData::NodeCompleted { output, finished }
            );
        }
    }

    #[tokio::test]
    async fn executes_parallel_branches_and_joins_their_outputs() {
        let mut builder =
            WorkflowMetadataBuilder::new("parallel", "Test parallel workflow execution");
        builder.start("a", ["b", "c"]).unwrap();
        builder
            .execute(
                "b",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: json!({"path": "{$input.paths.0}"}),
                },
                "d",
            )
            .unwrap();
        builder
            .execute(
                "c",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: json!({"path": "{$input.paths.1}"}),
                },
                "f",
            )
            .unwrap();
        builder
            .execute(
                "d",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: json!({"content": "{$b.content}"}),
                },
                "e",
            )
            .unwrap();
        builder
            .execute(
                "f",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: json!({"content": "{$c.content}"}),
                },
                "e",
            )
            .unwrap();
        builder
            .end(
                "e",
                Some(json!({
                    "first": "{$d.parsed}",
                    "second": "{$f.parsed}"
                })),
            )
            .unwrap();
        let plan_builder = WorkflowPlanBuilder::new(builder.build().unwrap());
        let (env, session) = WorkflowEnv::new("parallel", json!({"paths": ["one.txt", "two.txt"]}));
        let ctx = Ctx::new(Arc::new(ContextNull));
        let mut plan = plan_builder
            .build(crate::RT::null(), ctx.clone(), env)
            .await
            .unwrap();

        let PlanNext::Tasks(mut reads) = plan.init().await.unwrap() else {
            panic!("expected parallel read actions");
        };
        assert_eq!(reads.len(), 2);
        let b = TaskReq::<WorkflowActionRequest>::try_from_request(&mut reads[0]).unwrap();
        let c = TaskReq::<WorkflowActionRequest>::try_from_request(&mut reads[1]).unwrap();
        assert_eq!(b.req.payload, json!({"path": "one.txt"}));
        assert_eq!(c.req.payload, json!({"path": "two.txt"}));

        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: b.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"content": "first content"}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::Tasks(tasks) if tasks.is_empty()
        ));
        let PlanNext::Tasks(mut parses) = plan
            .next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: c.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"content": "second content"}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap()
        else {
            panic!("expected parallel parse actions");
        };
        assert_eq!(parses.len(), 2);
        let d = TaskReq::<WorkflowActionRequest>::try_from_request(&mut parses[0]).unwrap();
        let f = TaskReq::<WorkflowActionRequest>::try_from_request(&mut parses[1]).unwrap();
        assert_eq!(d.req.payload, json!({"content": "first content"}));
        assert_eq!(f.req.payload, json!({"content": "second content"}));

        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: d.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"parsed": "first result"}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::Tasks(tasks) if tasks.is_empty()
        ));
        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: f.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"parsed": "second result"}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::End
        ));
        let mut terminal = None;
        while let Some(event) = session.answer().await.unwrap() {
            if event.is_terminal() {
                terminal = Some(event);
                break;
            }
        }
        let terminal = terminal.unwrap();
        assert_eq!(terminal.node_id.as_deref(), Some("e"));
        assert_eq!(
            terminal.event_data().unwrap(),
            SessionEventData::NodeCompleted {
                output: json!({
                    "first": "first result",
                    "second": "second result"
                }),
                finished: true,
            }
        );
    }

    #[tokio::test]
    async fn execute_node_can_fan_out_to_multiple_successors() {
        let action = || WorkflowAction::Custom {
            task_type: "fixture".to_string(),
            request: Value::Null,
        };
        let mut builder =
            WorkflowMetadataBuilder::new("execute-fan-out", "Test execute node fan-out");
        builder.start("start", "source").unwrap();
        builder
            .execute("source", action(), ["left", "right"])
            .unwrap();
        builder.execute("left", action(), "end").unwrap();
        builder.execute("right", action(), "end").unwrap();
        builder
            .end(
                "end",
                Some(json!({
                    "left": "{$left}",
                    "right": "{$right}"
                })),
            )
            .unwrap();

        let plan_builder = WorkflowPlanBuilder::new(builder.build().unwrap());
        let (env, _) = WorkflowEnv::new("execute-fan-out", Value::Null);
        let ctx = Ctx::new(Arc::new(ContextNull));
        let mut plan = plan_builder
            .build(crate::RT::null(), ctx.clone(), env)
            .await
            .unwrap();

        let PlanNext::Tasks(mut source) = plan.init().await.unwrap() else {
            panic!("expected source action");
        };
        let source = TaskReq::<WorkflowActionRequest>::try_from_request(&mut source[0]).unwrap();
        let PlanNext::Tasks(mut branches) = plan
            .next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: source.meta,
                    resp: WorkflowActionResponse {
                        output: json!("source"),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap()
        else {
            panic!("expected fan-out actions");
        };
        assert_eq!(branches.len(), 2);
        let left = TaskReq::<WorkflowActionRequest>::try_from_request(&mut branches[0]).unwrap();
        let right = TaskReq::<WorkflowActionRequest>::try_from_request(&mut branches[1]).unwrap();

        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: left.meta,
                    resp: WorkflowActionResponse {
                        output: json!("L"),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::Tasks(tasks) if tasks.is_empty()
        ));
        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx,
                    meta: right.meta,
                    resp: WorkflowActionResponse { output: json!("R") },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::End
        ));
    }

    #[tokio::test]
    async fn decision_fan_out_marks_the_unselected_branch_inactive() {
        let action = || WorkflowAction::Custom {
            task_type: "fixture".to_string(),
            request: Value::Null,
        };
        let mut builder =
            WorkflowMetadataBuilder::new("decision-fan-out", "Test decision node fan-out");
        builder.start("start", "decision").unwrap();
        builder
            .decision(
                "decision",
                WorkflowCondition::Truthy { value: json!(true) },
                ["b", "c"],
                ["skipped"],
            )
            .unwrap();
        builder.execute("b", action(), "join").unwrap();
        builder.execute("c", action(), "join").unwrap();
        builder.execute("skipped", action(), "join").unwrap();
        builder
            .execute(
                "join",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: json!({
                        "b": "{$b.value}",
                        "c": "{$c.value}"
                    }),
                },
                "end",
            )
            .unwrap();
        builder.end("end", Some(json!("{$join}"))).unwrap();

        let plan_builder = WorkflowPlanBuilder::new(builder.build().unwrap());
        let (env, session) = WorkflowEnv::new("decision-fan-out", Value::Null);
        let ctx = Ctx::new(Arc::new(ContextNull));
        let mut plan = plan_builder
            .build(crate::RT::null(), ctx.clone(), env)
            .await
            .unwrap();

        let PlanNext::Tasks(mut selected) = plan.init().await.unwrap() else {
            panic!("expected selected decision branch tasks");
        };
        assert_eq!(selected.len(), 2);
        let b = TaskReq::<WorkflowActionRequest>::try_from_request(&mut selected[0]).unwrap();
        let c = TaskReq::<WorkflowActionRequest>::try_from_request(&mut selected[1]).unwrap();
        assert!(b.meta.id.contains("-b-"));
        assert!(c.meta.id.contains("-c-"));

        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: b.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"value": "B"}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::Tasks(tasks) if tasks.is_empty()
        ));
        let PlanNext::Tasks(mut joined) = plan
            .next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: c.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"value": "C"}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap()
        else {
            panic!("expected join action");
        };
        assert_eq!(joined.len(), 1);
        let join = TaskReq::<WorkflowActionRequest>::try_from_request(&mut joined[0]).unwrap();
        assert_eq!(join.req.payload, json!({"b": "B", "c": "C"}));

        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx,
                    meta: join.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"joined": true}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::End
        ));

        let mut completed_nodes = Vec::new();
        while let Some(event) = session.answer().await.unwrap() {
            if let Some(node_id) = &event.node_id {
                completed_nodes.push(node_id.clone());
            }
            if event.is_terminal() {
                break;
            }
        }
        assert!(!completed_nodes.iter().any(|node| node == "skipped"));
        assert_eq!(completed_nodes.last().map(String::as_str), Some("end"));
    }

    #[tokio::test]
    async fn forwards_single_agent_streaming_events() {
        let mut builder =
            WorkflowMetadataBuilder::new("single-agent-events", "Test single-agent node events");
        builder.start("start", "end").unwrap();
        builder.end("end", None).unwrap();
        let (_, session) = WorkflowEnv::new("single-agent-events", Value::Null);
        let (_, child_session) = SingleAgentEnv::new_with_session(
            crate::SingleAgentSource::AgentId("agent".to_string()),
            "input",
            session.clone(),
            "single-agent-events",
            "agent-node",
        );

        child_session
            .emit_agent(
                1,
                "agent",
                "model",
                SessionEventData::ModelOutput {
                    content: "hel".to_string(),
                },
            )
            .unwrap();
        let chunk = session.answer().await.unwrap().unwrap();
        assert_eq!(
            chunk.parament_plan_id.as_deref(),
            Some("single-agent-events")
        );
        assert_eq!(chunk.node_id.as_deref(), Some("agent-node"));
        assert_eq!(chunk.plan_id.as_deref(), Some("1"));
        assert_eq!(
            chunk.event_data().unwrap(),
            SessionEventData::ModelOutput {
                content: "hel".to_string()
            }
        );

        child_session
            .emit_agent(
                1,
                "agent",
                "agent",
                SessionEventData::Completed {
                    content: "hello".to_string(),
                },
            )
            .unwrap();
        let completed = session.answer().await.unwrap().unwrap();
        assert_eq!(
            completed.event_data().unwrap(),
            SessionEventData::Completed {
                content: "hello".to_string()
            }
        );
        assert!(!completed.is_terminal());
        assert_eq!(
            child_session
                .answer()
                .await
                .unwrap()
                .unwrap()
                .event_data()
                .unwrap(),
            SessionEventData::ModelOutput {
                content: "hel".to_string()
            }
        );
        assert_eq!(
            child_session
                .answer()
                .await
                .unwrap()
                .unwrap()
                .event_data()
                .unwrap(),
            SessionEventData::Completed {
                content: "hello".to_string()
            }
        );
    }
}
