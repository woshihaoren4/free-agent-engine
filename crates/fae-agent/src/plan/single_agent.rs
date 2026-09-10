use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCallChunk,
    ChatCompletionMessageToolCalls, ChatCompletionRequestAssistantMessage,
    ChatCompletionRequestAssistantMessageContent, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessage, ChatCompletionRequestToolMessage,
    ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage, ChatCompletionTools,
    CreateChatCompletionRequest, FunctionCall, FunctionObject,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, RwLock};
use tokio_stream::StreamExt;

use crate::{
    Ctx, McpQuery, McpRequest, McpResponse, McpToolInfo, ModelResponse, Plan, PlanBuilderWithEnv,
    PlanNext, RT, Session, SessionEvent, SessionEventData, SessionInput, SessionInputData,
    SessionMessage, SessionMessageRole, SessionOutput, SessionOutputChannel, SessionRequest,
    SessionResponse, SkillInfo, SkillQuery, TaskMeta, TaskReq, TaskRequest, TaskResp, TaskResponse,
    TaskType, ToolRequest, ToolRespItem, ToolResponse, WorkflowActionRequest,
    WorkflowActionResponse,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleAgentInfo {
    pub name: String,
    pub user_id: String,
    pub session_id: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleAgentModelConfig {
    pub model: String,
    #[serde(default = "default_context_size")]
    pub context_size: usize,
    pub history_turns: usize,
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleAgentConfig {
    pub agent: SingleAgentInfo,
    pub model: SingleAgentModelConfig,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub skills: Vec<SkillQuery>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SingleAgentSource {
    AgentId(String),
    Paths { config: PathBuf, prompt: PathBuf },
}

const fn default_max_tool_iterations() -> usize {
    8
}

const fn default_context_size() -> usize {
    32_000
}

pub const COMPRESSION_TASK_TYPE: &str = "workflow.compression";

#[derive(Debug)]
pub struct SingleAgentEnv {
    pub source: SingleAgentSource,
    pub input: String,
    session: CommonSession,
}

impl SingleAgentEnv {
    pub fn from_agent_id(
        agent_id: impl Into<String>,
        input: impl Into<String>,
    ) -> (Self, CommonSession) {
        Self::new(SingleAgentSource::AgentId(agent_id.into()), input)
    }

    pub fn from_paths(
        config: impl Into<PathBuf>,
        prompt: impl Into<PathBuf>,
        input: impl Into<String>,
    ) -> (Self, CommonSession) {
        Self::new(
            SingleAgentSource::Paths {
                config: config.into(),
                prompt: prompt.into(),
            },
            input,
        )
    }

    pub fn new(source: SingleAgentSource, input: impl Into<String>) -> (Self, CommonSession) {
        let session = CommonSession::new();
        (
            Self {
                source,
                input: input.into(),
                session: session.clone(),
            },
            session,
        )
    }

    pub fn session(&self) -> CommonSession {
        self.session.clone()
    }

    pub fn new_with_session(
        source: SingleAgentSource,
        input: impl Into<String>,
        workflow_session: CommonSession,
        workflow_id: impl Into<String>,
        node_id: impl Into<String>,
    ) -> (Self, CommonSession) {
        let session = CommonSession::new_in_workflow(workflow_session, workflow_id, node_id);
        (
            Self {
                source,
                input: input.into(),
                session: session.clone(),
            },
            session,
        )
    }
}

#[derive(Debug, Clone)]
pub struct CommonSession {
    inner: Arc<CommonSessionInner>,
    pub(crate) completion: Arc<CommonSessionCompletion>,
}

#[derive(Debug)]
struct CommonSessionInner {
    channel: SessionOutputChannel,
    workflow: Option<CommonSessionTarget>,
    binding: RwLock<Option<SingleAgentBinding>>,
    state: StdMutex<CommonSessionState>,
    idle: Notify,
    next_turn_id: AtomicU64,
}

#[derive(Debug, Clone)]
struct CommonSessionTarget {
    session: CommonSession,
    workflow_id: String,
    node_id: String,
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
    pub(crate) complete_context: AtomicBool,
}

impl Default for CommonSessionCompletion {
    fn default() -> Self {
        Self {
            result: StdMutex::new(None),
            notify: Notify::new(),
            complete_context: AtomicBool::new(true),
        }
    }
}

impl CommonSession {
    pub(crate) fn new() -> Self {
        Self::new_with_workflow(None)
    }

    fn new_in_workflow(
        session: CommonSession,
        workflow_id: impl Into<String>,
        node_id: impl Into<String>,
    ) -> Self {
        Self::new_with_workflow(Some(CommonSessionTarget {
            session,
            workflow_id: workflow_id.into(),
            node_id: node_id.into(),
        }))
    }

    fn new_with_workflow(workflow: Option<CommonSessionTarget>) -> Self {
        Self {
            inner: Arc::new(CommonSessionInner {
                channel: SessionOutputChannel::new(),
                workflow,
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
        source: impl Into<String>,
        data: SessionEventData,
    ) -> anyhow::Result<()> {
        let source = source.into();
        self.emit(SessionEvent::single_agent(
            turn_id,
            source.clone(),
            data.clone(),
        ))?;
        if let Some(workflow) = &self.inner.workflow {
            workflow.session.emit(SessionEvent::in_workflow(
                workflow.workflow_id.clone(),
                workflow.node_id.clone(),
                turn_id,
                source,
                data,
            ))?;
        }
        Ok(())
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

    async fn bind(&self, binding: SingleAgentBinding) -> anyhow::Result<u64> {
        let mut current = self.inner.binding.write().await;
        anyhow::ensure!(current.is_none(), "single-agent session is already bound");
        *current = Some(binding);
        self.activate_turn()
    }

    fn activate_turn(&self) -> anyhow::Result<u64> {
        let mut state = self.inner.state.lock().expect("session state poisoned");
        anyhow::ensure!(!state.active, "a turn is already running");
        state.active = true;
        state.accepting_input = true;
        Ok(self.inner.next_turn_id.fetch_add(1, Ordering::Relaxed))
    }

    fn take_pending_inputs(&self) -> Vec<SessionInputData> {
        let mut state = self.inner.state.lock().expect("session state poisoned");
        state.pending_inputs.drain(..).collect()
    }

    fn finish_or_take_pending(&self) -> Option<Vec<SessionInputData>> {
        let mut state = self.inner.state.lock().expect("session state poisoned");
        if state.pending_inputs.is_empty() {
            state.accepting_input = false;
            None
        } else {
            Some(state.pending_inputs.drain(..).collect())
        }
    }

    fn finish_turn(&self) {
        let mut state = self.inner.state.lock().expect("session state poisoned");
        state.active = false;
        state.accepting_input = false;
        state.cancel_requested = false;
        drop(state);
        self.inner.idle.notify_waiters();
    }

    fn cancel_requested(&self) -> bool {
        self.inner
            .state
            .lock()
            .expect("session state poisoned")
            .cancel_requested
    }

    fn abort_turn(&self) {
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
                idle.await;
            } else {
                break;
            }
        }

        let turn_id = self.inner.next_turn_id.fetch_add(1, Ordering::Relaxed);
        let plan = SingleAgentPlan::new(
            binding.ctx.clone(),
            binding.template,
            input.text,
            turn_id,
            self.clone(),
        );
        let task = TaskReq {
            ctx: binding.ctx,
            meta: TaskMeta {
                id: format!("single-agent-turn-{turn_id}"),
                ty: TaskType::Plan,
                ..Default::default()
            },
            req: Box::new(plan) as Box<dyn Plan>,
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

#[derive(Debug, Clone)]
pub struct SingleAgentPlanBuilder {
    home_dir: PathBuf,
}

impl Default for SingleAgentPlanBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SingleAgentPlanBuilder {
    pub fn new() -> Self {
        Self::with_home_dir(default_fae_home())
    }

    pub fn with_home_dir(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            home_dir: home_dir.into(),
        }
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub async fn load_config(
        &self,
        source: &SingleAgentSource,
    ) -> anyhow::Result<(SingleAgentConfig, String)> {
        let (config_path, prompt_path, expected_agent_id) = match source {
            SingleAgentSource::AgentId(agent_id) => {
                validate_agent_id(agent_id)?;
                let agents_dir = self.home_dir.join("agents");
                (
                    agents_dir.join(format!("{agent_id}_config.json")),
                    agents_dir.join(format!("{agent_id}_prompt.txt")),
                    Some(agent_id.as_str()),
                )
            }
            SingleAgentSource::Paths { config, prompt } => (config.clone(), prompt.clone(), None),
        };

        let config_bytes = tokio::fs::read(&config_path).await.map_err(|error| {
            anyhow::anyhow!(
                "load single-agent config `{}`: {error}",
                config_path.display()
            )
        })?;
        let config: SingleAgentConfig = serde_json::from_slice(&config_bytes).map_err(|error| {
            anyhow::anyhow!(
                "parse single-agent config `{}`: {error}",
                config_path.display()
            )
        })?;
        validate_config(&config)?;
        if let Some(agent_id) = expected_agent_id {
            anyhow::ensure!(
                config.agent.name == agent_id,
                "agent config `{}` contains name `{}`, expected `{agent_id}`",
                config_path.display(),
                config.agent.name
            );
        }
        let prompt = tokio::fs::read_to_string(&prompt_path)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "load single-agent prompt `{}`: {error}",
                    prompt_path.display()
                )
            })?;
        Ok((config, prompt))
    }
}

#[async_trait::async_trait]
impl PlanBuilderWithEnv<SingleAgentEnv> for SingleAgentPlanBuilder {
    async fn build(&self, rt: RT, ctx: Ctx, env: SingleAgentEnv) -> anyhow::Result<Box<dyn Plan>> {
        anyhow::ensure!(!env.input.trim().is_empty(), "input cannot be empty");
        let (config, prompt) = self.load_config(&env.source).await?;
        let skills = resolve_skills(&rt, &config.skills).await?;
        let prompt = prompt_with_skills(prompt, &skills);
        let (mut tool_definitions, mut tool_routes) = resolve_tools(&rt, &config.tools).await?;
        let (mcp_definitions, mcp_routes) = resolve_mcp_tools(&rt, &config.mcp_servers).await?;
        for (name, route) in mcp_routes {
            anyhow::ensure!(
                tool_routes.insert(name.clone(), route).is_none(),
                "multiple configured tools expose the model name `{name}`"
            );
        }
        tool_definitions.extend(mcp_definitions);
        let template = SingleAgentTemplate {
            agent: config.agent,
            prompt,
            model: config.model,
            tool_definitions,
            tool_routes,
        };
        let binding = SingleAgentBinding {
            rt: rt.clone(),
            ctx: ctx.clone(),
            template: template.clone(),
        };
        let turn_id = env.session.bind(binding).await?;

        Ok(Box::new(SingleAgentPlan::new(
            ctx,
            template,
            env.input,
            turn_id,
            env.session,
        )))
    }
}

fn validate_config(config: &SingleAgentConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        !config.agent.name.trim().is_empty(),
        "agent name cannot be empty"
    );
    anyhow::ensure!(
        !config.agent.user_id.trim().is_empty(),
        "user_id cannot be empty"
    );
    anyhow::ensure!(
        !config.agent.session_id.trim().is_empty(),
        "session_id cannot be empty"
    );
    anyhow::ensure!(
        !config.model.model.trim().is_empty(),
        "model cannot be empty"
    );
    anyhow::ensure!(
        config.model.context_size > 0,
        "context_size must be positive"
    );
    anyhow::ensure!(
        config.model.max_tool_iterations > 0,
        "max_tool_iterations must be positive"
    );
    Ok(())
}

fn validate_agent_id(agent_id: &str) -> anyhow::Result<()> {
    let mut components = Path::new(agent_id).components();
    anyhow::ensure!(
        !agent_id.is_empty()
            && matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none(),
        "agent id must be a single non-empty path component"
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

async fn resolve_tools(
    rt: &RT,
    tools: &[String],
) -> anyhow::Result<(Vec<ChatCompletionTools>, HashMap<String, CallableRoute>)> {
    let mut definitions = Vec::with_capacity(tools.len());
    let mut routes = HashMap::with_capacity(tools.len());
    for tool_name in tools {
        let value = rt
            .select::<_, Value>(TaskType::Tool, tool_name.clone())
            .await?;
        let function = serde_json::from_value::<FunctionObject>(value).map_err(|error| {
            anyhow::anyhow!("invalid description for tool `{tool_name}`: {error}")
        })?;
        anyhow::ensure!(
            routes
                .insert(
                    function.name.clone(),
                    CallableRoute::Tool(tool_name.clone())
                )
                .is_none(),
            "multiple configured tools expose the model name `{}`",
            function.name
        );
        definitions.push(ChatCompletionTools::Function(
            async_openai::types::chat::ChatCompletionTool { function },
        ));
    }
    Ok((definitions, routes))
}

async fn resolve_skills(rt: &RT, queries: &[SkillQuery]) -> anyhow::Result<Vec<SkillInfo>> {
    let mut skills = Vec::new();
    for query in queries {
        let mut found = rt
            .select::<_, Vec<SkillInfo>>(TaskType::Skill, query.clone())
            .await?;
        skills.append(&mut found);
    }
    Ok(skills)
}

fn prompt_with_skills(mut prompt: String, skills: &[SkillInfo]) -> String {
    if skills.is_empty() {
        return prompt;
    }
    prompt.push_str("\n\n## Available Skills\n");
    prompt.push_str("Read the matching SKILL.md file before applying a skill.\n");
    for skill in skills {
        prompt.push_str(&format!(
            "- {}: {} (path: {})\n",
            skill.name,
            skill.description,
            skill.path.display()
        ));
    }
    prompt
}

async fn resolve_mcp_tools(
    rt: &RT,
    servers: &[String],
) -> anyhow::Result<(Vec<ChatCompletionTools>, HashMap<String, CallableRoute>)> {
    let mut definitions = Vec::new();
    let mut routes = HashMap::new();
    for server in servers {
        let tools = rt
            .select::<_, Vec<McpToolInfo>>(TaskType::Mcp, McpQuery::new(server))
            .await?;
        for tool in tools {
            let model_name = tool.model_name();
            anyhow::ensure!(
                routes
                    .insert(
                        model_name.clone(),
                        CallableRoute::Mcp {
                            server: tool.server,
                            tool_name: tool.name,
                        },
                    )
                    .is_none(),
                "MCP tools expose duplicate model name `{model_name}`"
            );
            definitions.push(ChatCompletionTools::Function(
                async_openai::types::chat::ChatCompletionTool {
                    function: FunctionObject {
                        name: model_name,
                        description: (!tool.description.is_empty()).then_some(tool.description),
                        parameters: Some(tool.input_schema),
                        strict: None,
                    },
                },
            ));
        }
    }
    Ok((definitions, routes))
}

#[derive(Debug, Clone)]
enum CallableRoute {
    Tool(String),
    Mcp { server: String, tool_name: String },
}

#[derive(Debug, Clone)]
struct SingleAgentBinding {
    rt: RT,
    ctx: Ctx,
    template: SingleAgentTemplate,
}

#[derive(Debug, Clone)]
struct SingleAgentTemplate {
    agent: SingleAgentInfo,
    prompt: String,
    model: SingleAgentModelConfig,
    tool_definitions: Vec<ChatCompletionTools>,
    tool_routes: HashMap<String, CallableRoute>,
}

#[derive(Debug, Clone, Copy)]
enum PendingCallKind {
    Tool,
    Mcp,
}

#[derive(Debug)]
struct PendingCall {
    call_id: String,
    tool_name: String,
    kind: PendingCallKind,
}

#[derive(Debug)]
enum SingleAgentStage {
    History,
    Compression,
    Model,
    Tools { remaining: usize },
    Save,
}

#[derive(Debug)]
struct SingleAgentPlan {
    id: String,
    ctx: Ctx,
    template: SingleAgentTemplate,
    input: String,
    turn_id: u64,
    session: CommonSession,
    stage: SingleAgentStage,
    messages: Vec<ChatCompletionRequestMessage>,
    unsaved_messages: Vec<SessionMessage>,
    final_output: String,
    tool_iterations: usize,
    task_sequence: u64,
    pending_tools: HashMap<String, PendingCall>,
    owns_active_turn: bool,
    finish_on_drop: bool,
}

impl SingleAgentPlan {
    fn new(
        ctx: Ctx,
        template: SingleAgentTemplate,
        input: String,
        turn_id: u64,
        session: CommonSession,
    ) -> Self {
        let initial_message = SessionMessage::user(input.clone());
        Self {
            id: format!("single_agent-{}", wd_tools::uuid::v4()),
            ctx,
            template,
            input,
            turn_id,
            session,
            stage: SingleAgentStage::History,
            messages: Vec::new(),
            unsaved_messages: vec![initial_message],
            final_output: String::new(),
            tool_iterations: 0,
            task_sequence: 0,
            pending_tools: HashMap::new(),
            owns_active_turn: true,
            finish_on_drop: false,
        }
    }

    async fn emit(&self, source: impl Into<String>, data: SessionEventData) -> anyhow::Result<()> {
        self.session.emit_agent(self.turn_id, source, data)
    }

    fn task<Req: Send + 'static>(&mut self, ty: TaskType, req: Req) -> TaskRequest {
        self.task_sequence += 1;
        TaskReq {
            ctx: self.ctx.clone(),
            meta: TaskMeta {
                id: format!("single-agent-{}-{}", self.turn_id, self.task_sequence),
                ty,
                ..Default::default()
            },
            req,
        }
        .into_request()
    }

    fn history_task(&mut self) -> TaskRequest {
        self.task(
            TaskType::Session,
            SessionRequest::Query {
                user: self.template.agent.user_id.clone(),
                session_id: self.template.agent.session_id.clone(),
                limit: None,
                offset: None,
            },
        )
    }

    fn model_request(&self) -> CreateChatCompletionRequest {
        CreateChatCompletionRequest {
            model: self.template.model.model.clone(),
            messages: self.messages.clone(),
            stream: Some(true),
            max_completion_tokens: self.template.model.max_completion_tokens,
            temperature: self.template.model.temperature,
            tools: (!self.template.tool_definitions.is_empty())
                .then(|| self.template.tool_definitions.clone()),
            safety_identifier: Some(self.template.agent.user_id.clone()),
            ..Default::default()
        }
    }

    fn next_model_task(&mut self) -> anyhow::Result<TaskRequest> {
        let request = self.model_request();
        if estimated_tokens(&request) > self.template.model.context_size {
            let content = serde_json::to_string(
                &request
                    .messages
                    .iter()
                    .filter(|message| !matches!(message, ChatCompletionRequestMessage::System(_)))
                    .collect::<Vec<_>>(),
            )?;
            self.stage = SingleAgentStage::Compression;
            return Ok(self.task(
                TaskType::Any(COMPRESSION_TASK_TYPE.to_string()),
                WorkflowActionRequest {
                    action: "compression".to_string(),
                    payload: serde_json::json!({
                        "text": content,
                        "model": self.template.model.model,
                    }),
                },
            ));
        }

        self.stage = SingleAgentStage::Model;
        Ok(self.task(TaskType::Model, request))
    }

    fn apply_compression(&mut self, content: String) -> anyhow::Result<TaskRequest> {
        anyhow::ensure!(
            !content.trim().is_empty(),
            "compression runtime returned empty content"
        );
        let content = content.trim().to_string();
        self.messages
            .retain(|message| matches!(message, ChatCompletionRequestMessage::System(_)));
        self.messages.push(summary_chat_message(&content));
        self.unsaved_messages.push(SessionMessage::summary(content));

        let request = self.model_request();
        anyhow::ensure!(
            estimated_tokens(&request) <= self.template.model.context_size,
            "compressed model request still exceeds context_size ({})",
            self.template.model.context_size
        );
        self.stage = SingleAgentStage::Model;
        Ok(self.task(TaskType::Model, request))
    }

    fn save_task(&mut self) -> TaskRequest {
        self.task(
            TaskType::Session,
            SessionRequest::Add {
                user: self.template.agent.user_id.clone(),
                session_id: self.template.agent.session_id.clone(),
                messages: self.unsaved_messages.clone(),
            },
        )
    }

    async fn append_user_inputs(&mut self, inputs: Vec<SessionInputData>) -> anyhow::Result<()> {
        for input in inputs {
            let input = input.text;
            self.emit(
                self.template.agent.user_id.clone(),
                SessionEventData::UserInput {
                    content: input.clone(),
                },
            )
            .await?;
            self.messages.push(ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage {
                    content: input.clone().into(),
                    ..Default::default()
                },
            ));
            self.unsaved_messages.push(SessionMessage::user(input));
        }
        Ok(())
    }

    fn prepare_messages(&mut self, history: &[SessionMessage]) {
        self.messages.clear();
        if !self.template.prompt.is_empty() {
            self.messages.push(ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessage {
                    content: self.template.prompt.clone().into(),
                    ..Default::default()
                },
            ));
        }

        let history_limit = self.template.model.history_turns.saturating_mul(2);
        let history_start = history.len().saturating_sub(history_limit);
        let summary_start = history
            .iter()
            .rposition(|message| message.role == SessionMessageRole::Summary)
            .unwrap_or(0);
        let start = history_start.max(summary_start);
        for message in &history[start..] {
            self.messages.push(session_message_to_chat(message));
        }
        self.messages.push(ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                content: self.input.clone().into(),
                ..Default::default()
            },
        ));
    }

    async fn consume_model(
        &mut self,
        response: ModelResponse,
    ) -> anyhow::Result<(String, Vec<ChatCompletionMessageToolCalls>)> {
        match response {
            ModelResponse::Completed(response) => {
                let choice = response
                    .choices
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("model returned no choices"))?;
                let content = choice.message.content.unwrap_or_default();
                if !content.is_empty() {
                    self.emit(
                        self.template.model.model.clone(),
                        SessionEventData::ModelOutput {
                            content: content.clone(),
                        },
                    )
                    .await?;
                }
                Ok((content, choice.message.tool_calls.unwrap_or_default()))
            }
            ModelResponse::Streaming(mut stream) => {
                let mut content = String::new();
                let mut tool_calls = BTreeMap::<u32, ToolCallAccumulator>::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    for choice in chunk.choices {
                        if let Some(delta) = choice.delta.content.filter(|delta| !delta.is_empty())
                        {
                            content.push_str(&delta);
                            self.emit(
                                self.template.model.model.clone(),
                                SessionEventData::ModelOutput { content: delta },
                            )
                            .await?;
                        }
                        if let Some(reasoning) = choice
                            .delta
                            .reasoning_content
                            .filter(|reasoning| !reasoning.is_empty())
                        {
                            self.emit(
                                self.template.model.model.clone(),
                                SessionEventData::ModelReasoning { content: reasoning },
                            )
                            .await?;
                        }
                        for call in choice.delta.tool_calls.unwrap_or_default() {
                            tool_calls.entry(call.index).or_default().merge(call);
                        }
                    }
                }
                let tool_calls = tool_calls
                    .into_values()
                    .map(ToolCallAccumulator::finish)
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok((content, tool_calls))
            }
        }
    }

    async fn handle_model_response(&mut self, response: ModelResponse) -> anyhow::Result<PlanNext> {
        let (content, tool_calls) = self.consume_model(response).await?;
        if tool_calls.is_empty() {
            self.final_output = content.clone();
            self.messages.push(ChatCompletionRequestMessage::Assistant(
                ChatCompletionRequestAssistantMessage {
                    content: Some(content.clone().into()),
                    ..Default::default()
                },
            ));
            self.unsaved_messages
                .push(SessionMessage::assistant(content));

            let pending = self.session.take_pending_inputs();
            if !pending.is_empty() {
                self.append_user_inputs(pending).await?;
                return Ok(PlanNext::Tasks(vec![self.next_model_task()?]));
            }

            self.stage = SingleAgentStage::Save;
            return Ok(PlanNext::Tasks(vec![self.save_task()]));
        }

        self.tool_iterations += 1;
        anyhow::ensure!(
            self.tool_iterations <= self.template.model.max_tool_iterations,
            "model exceeded max_tool_iterations ({})",
            self.template.model.max_tool_iterations
        );

        let assistant_message = ChatCompletionRequestAssistantMessage {
            content: (!content.is_empty())
                .then_some(ChatCompletionRequestAssistantMessageContent::Text(content)),
            tool_calls: Some(tool_calls.clone()),
            ..Default::default()
        };
        self.messages
            .push(ChatCompletionRequestMessage::Assistant(assistant_message));

        let mut tasks = Vec::with_capacity(tool_calls.len());
        for call in tool_calls {
            let ChatCompletionMessageToolCalls::Function(call) = call else {
                anyhow::bail!("custom tool calls are not supported");
            };
            let call_id = call.id;
            let tool_name = call.function.name;
            let arguments = call.function.arguments;
            let route = self
                .template
                .tool_routes
                .get(&tool_name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("model requested unavailable tool `{tool_name}`"))?;
            self.emit(
                tool_name.clone(),
                SessionEventData::ToolCall {
                    call_id: call_id.clone(),
                    arguments: arguments.clone(),
                },
            )
            .await?;
            let (task, kind) = match route {
                CallableRoute::Tool(runtime_tool_name) => (
                    self.task(
                        TaskType::Tool,
                        ToolRequest::new(runtime_tool_name, arguments),
                    ),
                    PendingCallKind::Tool,
                ),
                CallableRoute::Mcp { server, tool_name } => (
                    self.task(TaskType::Mcp, McpRequest::new(server, tool_name, arguments)),
                    PendingCallKind::Mcp,
                ),
            };
            self.pending_tools.insert(
                task.meta.id.clone(),
                PendingCall {
                    call_id,
                    tool_name,
                    kind,
                },
            );
            tasks.push(task);
        }
        self.stage = SingleAgentStage::Tools {
            remaining: tasks.len(),
        };
        Ok(PlanNext::Tasks(tasks))
    }

    async fn handle_tool_response(
        &mut self,
        task_id: String,
        mut response: ToolResponse,
    ) -> anyhow::Result<PlanNext> {
        let pending = self
            .pending_tools
            .remove(&task_id)
            .ok_or_else(|| anyhow::anyhow!("unknown tool task `{task_id}`"))?;

        let completed_output = loop {
            match response.next().await? {
                ToolRespItem::Streaming(output) => {
                    self.emit(
                        pending.tool_name.clone(),
                        SessionEventData::ToolOutput {
                            call_id: pending.call_id.clone(),
                            output,
                            completed: false,
                        },
                    )
                    .await?;
                }
                ToolRespItem::Completed(output) => {
                    self.emit(
                        pending.tool_name.clone(),
                        SessionEventData::ToolOutput {
                            call_id: pending.call_id.clone(),
                            output: output.clone(),
                            completed: true,
                        },
                    )
                    .await?;
                    break output;
                }
            }
        };

        self.finish_tool_call(pending, completed_output).await
    }

    async fn handle_mcp_response(
        &mut self,
        task_id: String,
        response: McpResponse,
    ) -> anyhow::Result<PlanNext> {
        let pending = self
            .pending_tools
            .remove(&task_id)
            .ok_or_else(|| anyhow::anyhow!("unknown MCP task `{task_id}`"))?;
        self.emit(
            pending.tool_name.clone(),
            SessionEventData::ToolOutput {
                call_id: pending.call_id.clone(),
                output: response.output.clone(),
                completed: true,
            },
        )
        .await?;
        self.finish_tool_call(pending, response.output).await
    }

    async fn finish_tool_call(
        &mut self,
        pending: PendingCall,
        output: String,
    ) -> anyhow::Result<PlanNext> {
        self.messages.push(ChatCompletionRequestMessage::Tool(
            ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(output),
                tool_call_id: pending.call_id,
            },
        ));

        let SingleAgentStage::Tools { remaining } = &mut self.stage else {
            anyhow::bail!("received tool response outside tool stage");
        };
        *remaining -= 1;
        if *remaining == 0 {
            let pending = self.session.take_pending_inputs();
            self.append_user_inputs(pending).await?;
            Ok(PlanNext::Tasks(vec![self.next_model_task()?]))
        } else {
            Ok(PlanNext::Tasks(Vec::new()))
        }
    }
}

#[async_trait::async_trait]
impl Plan for SingleAgentPlan {
    fn id(&self) -> &str {
        &self.id
    }

    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        if self.session.cancel_requested() {
            self.finish_on_drop = true;
            return Ok(PlanNext::End);
        }
        self.emit(
            self.template.agent.name.clone(),
            SessionEventData::TurnStarted {
                input: self.input.clone(),
            },
        )
        .await?;
        Ok(PlanNext::Tasks(vec![self.history_task()]))
    }

    async fn next(&mut self, mut task_result: TaskResponse) -> anyhow::Result<PlanNext> {
        if self.session.cancel_requested() {
            self.finish_on_drop = true;
            return Ok(PlanNext::End);
        }
        match self.stage {
            SingleAgentStage::History => {
                let response = TaskResp::<SessionResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| anyhow::anyhow!("expected SessionResponse for history query"))?;
                let SessionResponse::History { messages, .. } = response.resp else {
                    anyhow::bail!("expected history query response");
                };
                self.prepare_messages(&messages);
                let pending = self.session.take_pending_inputs();
                self.append_user_inputs(pending).await?;
                Ok(PlanNext::Tasks(vec![self.next_model_task()?]))
            }
            SingleAgentStage::Compression => {
                let response =
                    TaskResp::<WorkflowActionResponse>::try_from_response(&mut task_result)
                        .ok_or_else(|| {
                            anyhow::anyhow!("expected WorkflowActionResponse after compression")
                        })?;
                let content = response.resp.output.as_str().ok_or_else(|| {
                    anyhow::anyhow!("compression runtime returned a non-string output")
                })?;
                Ok(PlanNext::Tasks(vec![
                    self.apply_compression(content.to_string())?,
                ]))
            }
            SingleAgentStage::Model => {
                let response = TaskResp::<ModelResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| anyhow::anyhow!("expected ModelResponse"))?;
                self.handle_model_response(response.resp).await
            }
            SingleAgentStage::Tools { .. } => {
                let task_id = task_result.meta.id.clone();
                let kind = self
                    .pending_tools
                    .get(&task_id)
                    .map(|pending| pending.kind)
                    .ok_or_else(|| anyhow::anyhow!("unknown tool task `{task_id}`"))?;
                match kind {
                    PendingCallKind::Tool => {
                        let response =
                            TaskResp::<ToolResponse>::try_from_response(&mut task_result)
                                .ok_or_else(|| anyhow::anyhow!("expected ToolResponse"))?;
                        self.handle_tool_response(task_id, response.resp).await
                    }
                    PendingCallKind::Mcp => {
                        let response = TaskResp::<McpResponse>::try_from_response(&mut task_result)
                            .ok_or_else(|| anyhow::anyhow!("expected McpResponse"))?;
                        self.handle_mcp_response(task_id, response.resp).await
                    }
                }
            }
            SingleAgentStage::Save => {
                let response = TaskResp::<SessionResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| anyhow::anyhow!("expected SessionResponse after save"))?;
                anyhow::ensure!(
                    matches!(response.resp, SessionResponse::Added { .. }),
                    "expected session add response"
                );
                self.unsaved_messages.clear();

                if let Some(pending) = self.session.finish_or_take_pending() {
                    self.append_user_inputs(pending).await?;
                    Ok(PlanNext::Tasks(vec![self.next_model_task()?]))
                } else {
                    self.finish_on_drop = true;
                    self.emit(
                        self.template.agent.name.clone(),
                        SessionEventData::Completed {
                            content: self.final_output.clone(),
                        },
                    )
                    .await?;
                    Ok(PlanNext::End)
                }
            }
        }
    }

    async fn abort(&mut self, _code: i32, error: String) {
        self.session.abort_turn();
        self.owns_active_turn = false;
        let _ = self
            .emit(
                self.template.agent.name.clone(),
                SessionEventData::Failed { error },
            )
            .await;
    }
}

impl Drop for SingleAgentPlan {
    fn drop(&mut self) {
        if self.owns_active_turn {
            if self.finish_on_drop {
                self.session.finish_turn();
            } else {
                self.session.abort_turn();
            }
        }
    }
}

#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCallAccumulator {
    fn merge(&mut self, chunk: ChatCompletionMessageToolCallChunk) {
        if let Some(id) = chunk.id {
            self.id.push_str(&id);
        }
        if let Some(function) = chunk.function {
            if let Some(name) = function.name {
                self.name.push_str(&name);
            }
            if let Some(arguments) = function.arguments {
                self.arguments.push_str(&arguments);
            }
        }
    }

    fn finish(self) -> anyhow::Result<ChatCompletionMessageToolCalls> {
        anyhow::ensure!(!self.id.is_empty(), "streamed tool call is missing id");
        anyhow::ensure!(!self.name.is_empty(), "streamed tool call is missing name");
        Ok(ChatCompletionMessageToolCalls::Function(
            ChatCompletionMessageToolCall {
                id: self.id,
                function: FunctionCall {
                    name: self.name,
                    arguments: self.arguments,
                },
            },
        ))
    }
}

fn estimated_tokens(value: &impl Serialize) -> usize {
    serde_json::to_string(value)
        .map(|json| json.chars().count().div_ceil(4).max(1))
        .unwrap_or(1)
}

fn summary_chat_message(content: &str) -> ChatCompletionRequestMessage {
    ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
        content: format!("Compressed conversation context:\n{content}").into(),
        ..Default::default()
    })
}

fn session_message_to_chat(message: &SessionMessage) -> ChatCompletionRequestMessage {
    match &message.role {
        SessionMessageRole::User => {
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: message.content.clone().into(),
                ..Default::default()
            })
        }
        SessionMessageRole::Assistant => {
            ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
                content: Some(message.content.clone().into()),
                ..Default::default()
            })
        }
        SessionMessageRole::Summary => summary_chat_message(&message.content),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::chat::{
        CreateChatCompletionResponse, FunctionCallStream, FunctionType,
    };

    fn test_config() -> SingleAgentConfig {
        SingleAgentConfig {
            agent: SingleAgentInfo {
                name: "reviewer".to_string(),
                user_id: "test-user".to_string(),
                session_id: "test-session".to_string(),
                metadata: HashMap::new(),
            },
            model: SingleAgentModelConfig {
                model: "test-model".to_string(),
                context_size: 8_192,
                history_turns: 10,
                max_completion_tokens: Some(1_024),
                temperature: Some(0.0),
                max_tool_iterations: 4,
            },
            tools: vec!["read_file".to_string()],
            skills: Vec::new(),
            mcp_servers: Vec::new(),
        }
    }

    #[test]
    fn model_config_defaults_context_size_to_32k() {
        let config: SingleAgentModelConfig = serde_json::from_value(serde_json::json!({
            "model": "test-model",
            "history_turns": 10
        }))
        .unwrap();

        assert_eq!(config.context_size, 32_000);
    }

    #[tokio::test]
    async fn builder_loads_agent_id_from_home_agents_directory() {
        let home = std::env::temp_dir().join(format!(
            "fae-single-agent-builder-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let agents = home.join("agents");
        tokio::fs::create_dir_all(&agents).await.unwrap();
        tokio::fs::write(
            agents.join("reviewer_config.json"),
            serde_json::to_vec(&test_config()).unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::write(agents.join("reviewer_prompt.txt"), "Review carefully.")
            .await
            .unwrap();

        let builder = SingleAgentPlanBuilder::with_home_dir(&home);
        let (config, prompt) = builder
            .load_config(&SingleAgentSource::AgentId("reviewer".to_string()))
            .await
            .unwrap();

        assert_eq!(config.agent.name, "reviewer");
        assert_eq!(config.model.model, "test-model");
        assert_eq!(config.tools, ["read_file"]);
        assert_eq!(prompt, "Review carefully.");
        tokio::fs::remove_dir_all(home).await.unwrap();
    }

    #[tokio::test]
    async fn builder_loads_explicit_config_and_prompt_paths() {
        let dir =
            std::env::temp_dir().join(format!("fae-single-agent-paths-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let config_path = dir.join("custom.json");
        let prompt_path = dir.join("prompt.txt");
        tokio::fs::write(&config_path, serde_json::to_vec(&test_config()).unwrap())
            .await
            .unwrap();
        tokio::fs::write(&prompt_path, "Custom prompt.")
            .await
            .unwrap();

        let builder = SingleAgentPlanBuilder::with_home_dir("/unused");
        let (config, prompt) = builder
            .load_config(&SingleAgentSource::Paths {
                config: config_path,
                prompt: prompt_path,
            })
            .await
            .unwrap();

        assert_eq!(config.agent.session_id, "test-session");
        assert_eq!(prompt, "Custom prompt.");
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }

    #[test]
    fn agent_id_rejects_path_components() {
        assert!(validate_agent_id("../reviewer").is_err());
        assert!(validate_agent_id("team/reviewer").is_err());
        assert!(validate_agent_id("reviewer").is_ok());
    }

    #[test]
    fn event_uses_common_envelope_and_nested_payload() {
        let event = SessionEvent::single_agent(
            7,
            "read_file",
            SessionEventData::ToolCall {
                call_id: "call-1".to_string(),
                arguments: "{\"path\":\"Cargo.toml\"}".to_string(),
            },
        );

        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "turn_id": 7,
                "name": "read_file",
                "type": "tool_call",
                "data": {
                    "call_id": "call-1",
                    "arguments": "{\"path\":\"Cargo.toml\"}"
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<SessionEvent>(value).unwrap(),
            event
        );
    }

    #[test]
    fn streamed_tool_call_chunks_are_assembled() {
        let mut call = ToolCallAccumulator::default();
        call.merge(ChatCompletionMessageToolCallChunk {
            index: 0,
            id: Some("call-1".to_string()),
            r#type: Some(FunctionType::Function),
            function: Some(FunctionCallStream {
                name: Some("read_".to_string()),
                arguments: Some("{\"path\":\"".to_string()),
            }),
        });
        call.merge(ChatCompletionMessageToolCallChunk {
            index: 0,
            id: None,
            r#type: None,
            function: Some(FunctionCallStream {
                name: Some("file".to_string()),
                arguments: Some("Cargo.toml\"}".to_string()),
            }),
        });

        let ChatCompletionMessageToolCalls::Function(call) = call.finish().unwrap() else {
            panic!("expected function tool call");
        };
        assert_eq!(call.id, "call-1");
        assert_eq!(call.function.name, "read_file");
        assert_eq!(call.function.arguments, "{\"path\":\"Cargo.toml\"}");
    }

    #[test]
    fn oversized_context_requests_compression_before_model() {
        let mut template = test_template();
        template.model.context_size = 1;
        let mut plan = SingleAgentPlan::new(
            Ctx::null(),
            template,
            "a long input that exceeds the configured context".to_string(),
            1,
            CommonSession::new(),
        );
        plan.prepare_messages(&[]);

        let mut task = plan.next_model_task().unwrap();

        assert!(matches!(plan.stage, SingleAgentStage::Compression));
        assert_eq!(
            task.meta.ty,
            TaskType::Any(COMPRESSION_TASK_TYPE.to_string())
        );
        let request = TaskReq::<WorkflowActionRequest>::try_from_request(&mut task).unwrap();
        assert_eq!(request.req.action, "compression");
        assert_eq!(request.req.payload["model"], "test-model");
        assert!(
            request.req.payload["text"]
                .as_str()
                .unwrap()
                .contains("long input")
        );
    }

    #[test]
    fn compressed_content_replaces_context_and_is_saved() {
        let mut plan = SingleAgentPlan::new(
            Ctx::null(),
            test_template(),
            "current question".to_string(),
            1,
            CommonSession::new(),
        );
        plan.prepare_messages(&[
            SessionMessage::user("old question"),
            SessionMessage::assistant("old answer"),
        ]);

        let mut task = plan
            .apply_compression("condensed history and current question".to_string())
            .unwrap();

        assert!(matches!(plan.stage, SingleAgentStage::Model));
        assert_eq!(
            plan.unsaved_messages,
            vec![
                SessionMessage::user("current question"),
                SessionMessage::summary("condensed history and current question")
            ]
        );
        let request = TaskReq::<CreateChatCompletionRequest>::try_from_request(&mut task).unwrap();
        assert_eq!(request.req.messages.len(), 2);
        assert!(matches!(
            request.req.messages.first(),
            Some(ChatCompletionRequestMessage::System(_))
        ));
        assert!(matches!(
            request.req.messages.last(),
            Some(ChatCompletionRequestMessage::User(message))
                if serde_json::to_string(&message.content)
                    .unwrap()
                    .contains("condensed history")
        ));
    }

    #[test]
    fn history_loading_stops_at_latest_summary() {
        let mut plan = SingleAgentPlan::new(
            Ctx::null(),
            test_template(),
            "latest question".to_string(),
            1,
            CommonSession::new(),
        );

        plan.prepare_messages(&[
            SessionMessage::user("discarded question"),
            SessionMessage::assistant("discarded answer"),
            SessionMessage::summary("compressed history"),
            SessionMessage::assistant("answer after summary"),
        ]);

        let serialized = serde_json::to_string(&plan.messages).unwrap();
        assert!(!serialized.contains("discarded question"));
        assert!(!serialized.contains("discarded answer"));
        assert!(serialized.contains("compressed history"));
        assert!(serialized.contains("answer after summary"));
        assert!(serialized.contains("latest question"));
    }

    #[test]
    fn skill_metadata_is_added_to_the_system_prompt() {
        let prompt = prompt_with_skills(
            "base prompt".to_string(),
            &[SkillInfo {
                name: "review".to_string(),
                description: "Review Rust code".to_string(),
                path: "/tmp/review/SKILL.md".into(),
                version: None,
                metadata: None,
            }],
        );

        assert!(prompt.contains("review: Review Rust code"));
        assert!(prompt.contains("/tmp/review/SKILL.md"));
    }

    #[tokio::test]
    async fn mcp_tool_calls_use_the_mcp_task_type_and_response() -> anyhow::Result<()> {
        let session = CommonSession::new();
        session.activate_turn().unwrap();
        let ctx = Ctx::null();
        let mut template = test_template();
        template.tool_routes.insert(
            "maps__search".to_string(),
            CallableRoute::Mcp {
                server: "maps".to_string(),
                tool_name: "search".to_string(),
            },
        );
        let mut plan = SingleAgentPlan::new(ctx.clone(), template, "find it".into(), 1, session);
        plan.stage = SingleAgentStage::Model;

        let response: CreateChatCompletionResponse = serde_json::from_value(serde_json::json!({
            "id": "response-1",
            "choices": [{
                "index": 0,
                "message": {
                    "content": null,
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "maps__search",
                            "arguments": "{\"query\":\"park\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "created": 0,
            "model": "test-model",
            "object": "chat.completion",
            "usage": null
        }))?;
        let PlanNext::Tasks(mut tasks) = plan
            .handle_model_response(ModelResponse::Completed(response))
            .await?
        else {
            panic!("expected MCP task");
        };
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].meta.ty, TaskType::Mcp);
        let request = TaskReq::<McpRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert_eq!(request.req.server, "maps");
        assert_eq!(request.req.tool_name, "search");

        let next = plan
            .next(
                TaskResp {
                    ctx,
                    meta: request.meta,
                    resp: McpResponse {
                        output: "{\"content\":[]}".to_string(),
                    },
                }
                .into_response(),
            )
            .await?;
        assert!(matches!(next, PlanNext::Tasks(tasks) if tasks.len() == 1));
        assert!(matches!(
            plan.messages.last(),
            Some(ChatCompletionRequestMessage::Tool(message))
                if message.tool_call_id == "call-1"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn session_rejects_calls_before_binding() {
        let session = CommonSession::new();
        let error = session
            .call(SessionInput::NewChat("hello".into()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not bound"));
    }

    #[tokio::test]
    async fn new_chat_cancels_the_active_plan_before_starting_another() {
        let session = CommonSession::new();
        let ctx = Ctx::null();
        let template = test_template();
        let turn_id = session
            .bind(SingleAgentBinding {
                rt: RT::null(),
                ctx: ctx.clone(),
                template: template.clone(),
            })
            .await
            .unwrap();
        let mut plan =
            SingleAgentPlan::new(ctx, template, "first".to_string(), turn_id, session.clone());

        let next_session = session.clone();
        let next_chat = tokio::spawn(async move {
            next_session
                .call(SessionInput::NewChat("second".into()))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !session.cancel_requested() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        assert!(matches!(plan.init().await.unwrap(), PlanNext::End));
        drop(plan);

        tokio::time::timeout(std::time::Duration::from_secs(1), next_chat)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
    }

    #[test]
    fn plans_have_unique_ids() {
        let first = SingleAgentPlan::new(
            Ctx::null(),
            test_template(),
            "first".to_string(),
            1,
            CommonSession::new(),
        );
        let second = SingleAgentPlan::new(
            Ctx::null(),
            test_template(),
            "second".to_string(),
            2,
            CommonSession::new(),
        );

        assert!(first.id().starts_with("single_agent-"));
        assert!(second.id().starts_with("single_agent-"));
        assert_ne!(first.id(), second.id());
    }

    #[tokio::test]
    async fn active_plan_appends_new_call_to_current_messages() {
        let session = CommonSession::new();
        let ctx = Ctx::null();
        let template = test_template();
        let turn_id = session
            .bind(SingleAgentBinding {
                rt: RT::null(),
                ctx: ctx.clone(),
                template: template.clone(),
            })
            .await
            .unwrap();
        let mut plan = SingleAgentPlan::new(
            ctx.clone(),
            template,
            "first".to_string(),
            turn_id,
            session.clone(),
        );

        session
            .call(SessionInput::Supplement("second".into()))
            .await
            .unwrap();
        plan.init().await.unwrap();
        let history_response = TaskResp {
            ctx,
            meta: TaskMeta::default(),
            resp: SessionResponse::History {
                path: "session.jsonl".into(),
                messages: Vec::new(),
            },
        }
        .into_response();
        assert!(matches!(
            plan.next(history_response).await.unwrap(),
            PlanNext::Tasks(_)
        ));

        let user_messages = plan
            .messages
            .iter()
            .filter_map(|message| match message {
                ChatCompletionRequestMessage::User(message) => match &message.content {
                    async_openai::types::chat::ChatCompletionRequestUserMessageContent::Text(
                        content,
                    ) => Some(content.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(user_messages, vec!["first", "second"]);
        assert_eq!(
            plan.unsaved_messages,
            vec![
                SessionMessage::user("first"),
                SessionMessage::user("second")
            ]
        );
    }

    #[tokio::test]
    async fn active_plan_appends_new_input_after_tool_result() {
        let session = CommonSession::new();
        let ctx = Ctx::null();
        let mut template = test_template();
        template.tool_routes.insert(
            "read_file".to_string(),
            CallableRoute::Tool("read_file".to_string()),
        );
        let turn_id = session
            .bind(SingleAgentBinding {
                rt: RT::null(),
                ctx: ctx.clone(),
                template: template.clone(),
            })
            .await
            .unwrap();
        let mut plan = SingleAgentPlan::new(
            ctx.clone(),
            template,
            "first".to_string(),
            turn_id,
            session.clone(),
        );

        plan.init().await.unwrap();
        plan.next(
            TaskResp {
                ctx: ctx.clone(),
                meta: TaskMeta::default(),
                resp: SessionResponse::History {
                    path: "session.jsonl".into(),
                    messages: Vec::new(),
                },
            }
            .into_response(),
        )
        .await
        .unwrap();

        let model_response: CreateChatCompletionResponse =
            serde_json::from_value(serde_json::json!({
                "id": "response-1",
                "choices": [{
                    "index": 0,
                    "message": {
                        "content": null,
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call-1",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments": "{\"path\":\"Cargo.toml\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "created": 0,
                "model": "test-model",
                "object": "chat.completion",
                "usage": null
            }))
            .unwrap();
        let PlanNext::Tasks(tool_tasks) = plan
            .next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: TaskMeta::default(),
                    resp: ModelResponse::Completed(model_response),
                }
                .into_response(),
            )
            .await
            .unwrap()
        else {
            panic!("expected tool task");
        };
        let tool_task_id = tool_tasks[0].meta.id.clone();

        session
            .call(SessionInput::Supplement("second".into()))
            .await
            .unwrap();
        let next = plan
            .next(
                TaskResp {
                    ctx,
                    meta: TaskMeta {
                        id: tool_task_id,
                        ..Default::default()
                    },
                    resp: ToolResponse::with_result("{\"content\":\"workspace\"}".to_string()),
                }
                .into_response(),
            )
            .await
            .unwrap();

        assert!(matches!(next, PlanNext::Tasks(_)));
        assert!(matches!(
            plan.messages.as_slice(),
            [
                ChatCompletionRequestMessage::System(_),
                ChatCompletionRequestMessage::User(_),
                ChatCompletionRequestMessage::Assistant(_),
                ChatCompletionRequestMessage::Tool(_),
                ChatCompletionRequestMessage::User(_)
            ]
        ));
        assert_eq!(
            plan.unsaved_messages,
            vec![
                SessionMessage::user("first"),
                SessionMessage::user("second")
            ]
        );
    }

    #[tokio::test]
    async fn no_tool_turn_streams_and_completes() {
        let session = CommonSession::new();
        session.activate_turn().unwrap();
        let ctx = Ctx::null();
        let template = test_template();
        let mut plan = SingleAgentPlan::new(
            ctx.clone(),
            template,
            "hello".to_string(),
            1,
            session.clone(),
        );

        assert!(matches!(plan.init().await.unwrap(), PlanNext::Tasks(_)));
        let history_response = TaskResp {
            ctx: ctx.clone(),
            meta: TaskMeta::default(),
            resp: SessionResponse::History {
                path: "session.jsonl".into(),
                messages: Vec::new(),
            },
        }
        .into_response();
        assert!(matches!(
            plan.next(history_response).await.unwrap(),
            PlanNext::Tasks(_)
        ));

        let response: CreateChatCompletionResponse = serde_json::from_value(serde_json::json!({
            "id": "response-1",
            "choices": [{
                "index": 0,
                "message": {"content": "hi", "role": "assistant"},
                "finish_reason": "stop"
            }],
            "created": 0,
            "model": "test-model",
            "object": "chat.completion",
            "usage": null
        }))
        .unwrap();
        let model_response = TaskResp {
            ctx: ctx.clone(),
            meta: TaskMeta::default(),
            resp: ModelResponse::Completed(response),
        }
        .into_response();
        assert!(matches!(
            plan.next(model_response).await.unwrap(),
            PlanNext::Tasks(_)
        ));

        let save_response = TaskResp {
            ctx,
            meta: TaskMeta::default(),
            resp: SessionResponse::Added {
                path: "session.jsonl".into(),
                added: 2,
            },
        }
        .into_response();
        assert!(matches!(
            plan.next(save_response).await.unwrap(),
            PlanNext::End
        ));

        let mut kinds = Vec::new();
        loop {
            let event = session.answer().await.unwrap().unwrap();
            kinds.push(event.kind().to_string());
            if event.is_terminal() {
                break;
            }
        }
        assert_eq!(kinds, vec!["turn_started", "model_output", "completed"]);
    }

    fn test_template() -> SingleAgentTemplate {
        SingleAgentTemplate {
            agent: SingleAgentInfo {
                name: "test-agent".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-1".to_string(),
                metadata: HashMap::new(),
            },
            prompt: "be concise".to_string(),
            model: SingleAgentModelConfig {
                model: "test-model".to_string(),
                context_size: 1_024,
                history_turns: 2,
                max_completion_tokens: None,
                temperature: None,
                max_tool_iterations: 2,
            },
            tool_definitions: Vec::new(),
            tool_routes: HashMap::new(),
        }
    }
}
