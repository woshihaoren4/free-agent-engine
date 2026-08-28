use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
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
use tokio::sync::{
    Mutex, RwLock,
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};
use tokio_stream::StreamExt;

use crate::{
    Ctx, ModelResponse, Plan, PlanBuilderWithEnv, PlanNext, RT, Session, SessionMessage,
    SessionMessageRole, SessionRequest, SessionResponse, TaskMeta, TaskReq, TaskRequest, TaskResp,
    TaskResponse, TaskType, ToolRequest, ToolRespItem, ToolResponse, common,
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
    pub context_size: usize,
    pub history_turns: usize,
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: usize,
}

const fn default_max_tool_iterations() -> usize {
    8
}

#[derive(Debug)]
pub struct SingleAgentEnv {
    pub agent: SingleAgentInfo,
    pub prompt: String,
    pub model: SingleAgentModelConfig,
    pub input: String,
    pub tools: Vec<String>,
    session: SingleAgentSession,
}

impl SingleAgentEnv {
    pub fn new(
        agent: SingleAgentInfo,
        prompt: impl Into<String>,
        model: SingleAgentModelConfig,
        input: impl Into<String>,
        tools: Vec<String>,
    ) -> (Self, SingleAgentSession) {
        let session = SingleAgentSession::new();
        (
            Self {
                agent,
                prompt: prompt.into(),
                model,
                input: input.into(),
                tools,
                session: session.clone(),
            },
            session,
        )
    }

    pub fn session(&self) -> SingleAgentSession {
        self.session.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SingleAgentEvent {
    TurnStarted {
        turn_id: u64,
        name: String,
        input: String,
    },
    HistoryLoaded {
        turn_id: u64,
        name: String,
        messages: Vec<SessionMessage>,
    },
    ModelOutput {
        turn_id: u64,
        name: String,
        content: String,
    },
    ModelReasoning {
        turn_id: u64,
        name: String,
        content: String,
    },
    ToolCall {
        turn_id: u64,
        name: String,
        call_id: String,
        arguments: String,
    },
    ToolOutput {
        turn_id: u64,
        name: String,
        call_id: String,
        output: String,
        completed: bool,
    },
    Completed {
        turn_id: u64,
        name: String,
        content: String,
    },
    Failed {
        turn_id: u64,
        name: String,
        error: String,
    },
}

impl SingleAgentEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::TurnStarted { .. } => "turn_started",
            Self::HistoryLoaded { .. } => "history_loaded",
            Self::ModelOutput { .. } => "model_output",
            Self::ModelReasoning { .. } => "model_reasoning",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolOutput { .. } => "tool_output",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::TurnStarted { name, .. }
            | Self::HistoryLoaded { name, .. }
            | Self::ModelOutput { name, .. }
            | Self::ModelReasoning { name, .. }
            | Self::ToolCall { name, .. }
            | Self::ToolOutput { name, .. }
            | Self::Completed { name, .. }
            | Self::Failed { name, .. } => name,
        }
    }

    pub fn turn_id(&self) -> u64 {
        match self {
            Self::TurnStarted { turn_id, .. }
            | Self::HistoryLoaded { turn_id, .. }
            | Self::ModelOutput { turn_id, .. }
            | Self::ModelReasoning { turn_id, .. }
            | Self::ToolCall { turn_id, .. }
            | Self::ToolOutput { turn_id, .. }
            | Self::Completed { turn_id, .. }
            | Self::Failed { turn_id, .. } => *turn_id,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. })
    }
}

#[derive(Debug, Clone)]
pub struct SingleAgentSession {
    inner: Arc<SingleAgentSessionInner>,
}

#[derive(Debug)]
struct SingleAgentSessionInner {
    sender: UnboundedSender<SingleAgentEvent>,
    receiver: Mutex<UnboundedReceiver<SingleAgentEvent>>,
    binding: RwLock<Option<SingleAgentBinding>>,
    active: AtomicBool,
    next_turn_id: AtomicU64,
}

impl SingleAgentSession {
    fn new() -> Self {
        let (sender, receiver) = unbounded_channel();
        Self {
            inner: Arc::new(SingleAgentSessionInner {
                sender,
                receiver: Mutex::new(receiver),
                binding: RwLock::new(None),
                active: AtomicBool::new(false),
                next_turn_id: AtomicU64::new(1),
            }),
        }
    }

    async fn bind(&self, binding: SingleAgentBinding) -> anyhow::Result<u64> {
        let mut current = self.inner.binding.write().await;
        anyhow::ensure!(current.is_none(), "single-agent session is already bound");
        *current = Some(binding);
        self.activate_turn()
    }

    fn activate_turn(&self) -> anyhow::Result<u64> {
        self.inner
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("a turn is already running"))?;
        Ok(self.inner.next_turn_id.fetch_add(1, Ordering::Relaxed))
    }

    fn finish_turn(&self) {
        self.inner.active.store(false, Ordering::Release);
    }
}

#[async_trait::async_trait]
impl Session<String, SingleAgentEvent> for SingleAgentSession {
    async fn call(&self, input: String) -> anyhow::Result<()> {
        anyhow::ensure!(!input.trim().is_empty(), "input cannot be empty");
        let binding = self
            .inner
            .binding
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("single-agent session is not bound to an engine"))?;
        let turn_id = self.activate_turn()?;
        let plan = SingleAgentPlan::new(
            binding.ctx.clone(),
            binding.template,
            input,
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
            self.finish_turn();
            return Err(error);
        }
        Ok(())
    }

    async fn answer(&self) -> anyhow::Result<Option<SingleAgentEvent>> {
        Ok(self.inner.receiver.lock().await.recv().await)
    }
}

#[derive(Debug, Default)]
pub struct SingleAgentPlanBuilder;

#[async_trait::async_trait]
impl PlanBuilderWithEnv<SingleAgentEnv> for SingleAgentPlanBuilder {
    async fn build(&self, rt: RT, ctx: Ctx, env: SingleAgentEnv) -> anyhow::Result<Box<dyn Plan>> {
        validate_env(&env)?;
        let (tool_definitions, tool_routes) = resolve_tools(&rt, &env.tools).await?;
        let template = SingleAgentTemplate {
            agent: env.agent,
            prompt: env.prompt,
            model: env.model,
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

fn validate_env(env: &SingleAgentEnv) -> anyhow::Result<()> {
    anyhow::ensure!(
        !env.agent.name.trim().is_empty(),
        "agent name cannot be empty"
    );
    anyhow::ensure!(
        !env.agent.user_id.trim().is_empty(),
        "user_id cannot be empty"
    );
    anyhow::ensure!(
        !env.agent.session_id.trim().is_empty(),
        "session_id cannot be empty"
    );
    anyhow::ensure!(!env.model.model.trim().is_empty(), "model cannot be empty");
    anyhow::ensure!(env.model.context_size > 0, "context_size must be positive");
    anyhow::ensure!(
        env.model.max_tool_iterations > 0,
        "max_tool_iterations must be positive"
    );
    anyhow::ensure!(!env.input.trim().is_empty(), "input cannot be empty");
    Ok(())
}

async fn resolve_tools(
    rt: &RT,
    tools: &[String],
) -> anyhow::Result<(Vec<ChatCompletionTools>, HashMap<String, String>)> {
    let mut definitions = Vec::with_capacity(tools.len());
    let mut routes = HashMap::with_capacity(tools.len());
    for tool_name in tools {
        let mut condition: common::AnyType = Box::new(tool_name.clone());
        let value = rt.select(TaskType::Tool, &mut condition).await?;
        let value = *value.downcast::<Value>().map_err(|_| {
            anyhow::anyhow!("tool `{tool_name}` description was not serde_json::Value")
        })?;
        let function = serde_json::from_value::<FunctionObject>(value).map_err(|error| {
            anyhow::anyhow!("invalid description for tool `{tool_name}`: {error}")
        })?;
        anyhow::ensure!(
            routes
                .insert(function.name.clone(), tool_name.clone())
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
    tool_routes: HashMap<String, String>,
}

#[derive(Debug)]
enum SingleAgentStage {
    History,
    Model,
    Tools { remaining: usize },
    Save,
}

#[derive(Debug)]
struct SingleAgentPlan {
    ctx: Ctx,
    template: SingleAgentTemplate,
    input: String,
    turn_id: u64,
    session: SingleAgentSession,
    stage: SingleAgentStage,
    messages: Vec<ChatCompletionRequestMessage>,
    final_output: String,
    tool_iterations: usize,
    task_sequence: u64,
    pending_tools: HashMap<String, (String, String)>,
}

impl SingleAgentPlan {
    fn new(
        ctx: Ctx,
        template: SingleAgentTemplate,
        input: String,
        turn_id: u64,
        session: SingleAgentSession,
    ) -> Self {
        Self {
            ctx,
            template,
            input,
            turn_id,
            session,
            stage: SingleAgentStage::History,
            messages: Vec::new(),
            final_output: String::new(),
            tool_iterations: 0,
            task_sequence: 0,
            pending_tools: HashMap::new(),
        }
    }

    async fn emit(&self, event: SingleAgentEvent) -> anyhow::Result<()> {
        self.session
            .inner
            .sender
            .send(event)
            .map_err(|error| anyhow::anyhow!("send single-agent event failed: {error}"))
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

    fn model_task(&mut self) -> anyhow::Result<TaskRequest> {
        let mut request = CreateChatCompletionRequest {
            model: self.template.model.model.clone(),
            messages: self.messages.clone(),
            stream: Some(true),
            max_completion_tokens: self.template.model.max_completion_tokens,
            temperature: self.template.model.temperature,
            tools: (!self.template.tool_definitions.is_empty())
                .then(|| self.template.tool_definitions.clone()),
            safety_identifier: Some(self.template.agent.user_id.clone()),
            ..Default::default()
        };
        request.stream = Some(true);
        Ok(self.task(TaskType::Model, request))
    }

    fn save_task(&mut self) -> TaskRequest {
        self.task(
            TaskType::Session,
            SessionRequest::Add {
                user: self.template.agent.user_id.clone(),
                session_id: self.template.agent.session_id.clone(),
                messages: vec![
                    SessionMessage::user(self.input.clone()),
                    SessionMessage::assistant(self.final_output.clone()),
                ],
            },
        )
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
        let start = history.len().saturating_sub(history_limit);
        for message in &history[start..] {
            match message.role {
                SessionMessageRole::User => {
                    self.messages.push(ChatCompletionRequestMessage::User(
                        ChatCompletionRequestUserMessage {
                            content: message.content.clone().into(),
                            ..Default::default()
                        },
                    ));
                }
                SessionMessageRole::Assistant => {
                    self.messages.push(ChatCompletionRequestMessage::Assistant(
                        ChatCompletionRequestAssistantMessage {
                            content: Some(message.content.clone().into()),
                            ..Default::default()
                        },
                    ));
                }
            }
        }
        self.messages.push(ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                content: self.input.clone().into(),
                ..Default::default()
            },
        ));
        trim_messages_to_context(&mut self.messages, self.template.model.context_size);
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
                    self.emit(SingleAgentEvent::ModelOutput {
                        turn_id: self.turn_id,
                        name: self.template.model.model.clone(),
                        content: content.clone(),
                    })
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
                        if let Some(delta) = choice.delta.content {
                            content.push_str(&delta);
                            self.emit(SingleAgentEvent::ModelOutput {
                                turn_id: self.turn_id,
                                name: self.template.model.model.clone(),
                                content: delta,
                            })
                            .await?;
                        }
                        if let Some(reasoning) = choice.delta.reasoning_content {
                            self.emit(SingleAgentEvent::ModelReasoning {
                                turn_id: self.turn_id,
                                name: self.template.model.model.clone(),
                                content: reasoning,
                            })
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
            self.final_output = content;
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
            let runtime_tool_name = self
                .template
                .tool_routes
                .get(&tool_name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("model requested unavailable tool `{tool_name}`"))?;
            self.emit(SingleAgentEvent::ToolCall {
                turn_id: self.turn_id,
                name: tool_name.clone(),
                call_id: call_id.clone(),
                arguments: arguments.clone(),
            })
            .await?;
            let task = self.task(
                TaskType::Tool,
                ToolRequest::new(runtime_tool_name, arguments),
            );
            self.pending_tools
                .insert(task.meta.id.clone(), (call_id, tool_name));
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
        let (call_id, tool_name) = self
            .pending_tools
            .remove(&task_id)
            .ok_or_else(|| anyhow::anyhow!("unknown tool task `{task_id}`"))?;

        let completed_output = loop {
            match response.next().await? {
                ToolRespItem::Streaming(output) => {
                    self.emit(SingleAgentEvent::ToolOutput {
                        turn_id: self.turn_id,
                        name: tool_name.clone(),
                        call_id: call_id.clone(),
                        output,
                        completed: false,
                    })
                    .await?;
                }
                ToolRespItem::Completed(output) => {
                    self.emit(SingleAgentEvent::ToolOutput {
                        turn_id: self.turn_id,
                        name: tool_name,
                        call_id: call_id.clone(),
                        output: output.clone(),
                        completed: true,
                    })
                    .await?;
                    break output;
                }
            }
        };

        self.messages.push(ChatCompletionRequestMessage::Tool(
            ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(completed_output),
                tool_call_id: call_id,
            },
        ));

        let SingleAgentStage::Tools { remaining } = &mut self.stage else {
            anyhow::bail!("received tool response outside tool stage");
        };
        *remaining -= 1;
        if *remaining == 0 {
            self.stage = SingleAgentStage::Model;
            Ok(PlanNext::Tasks(vec![self.model_task()?]))
        } else {
            Ok(PlanNext::Tasks(Vec::new()))
        }
    }
}

#[async_trait::async_trait]
impl Plan for SingleAgentPlan {
    fn id(&self) -> &str {
        "single_agent"
    }

    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        self.emit(SingleAgentEvent::TurnStarted {
            turn_id: self.turn_id,
            name: self.template.agent.name.clone(),
            input: self.input.clone(),
        })
        .await?;
        Ok(PlanNext::Tasks(vec![self.history_task()]))
    }

    async fn next(&mut self, mut task_result: TaskResponse) -> anyhow::Result<PlanNext> {
        match self.stage {
            SingleAgentStage::History => {
                let response = TaskResp::<SessionResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| anyhow::anyhow!("expected SessionResponse for history query"))?;
                let SessionResponse::History { messages, .. } = response.resp else {
                    anyhow::bail!("expected history query response");
                };
                self.emit(SingleAgentEvent::HistoryLoaded {
                    turn_id: self.turn_id,
                    name: "session".to_string(),
                    messages: messages.clone(),
                })
                .await?;
                self.prepare_messages(&messages);
                self.stage = SingleAgentStage::Model;
                Ok(PlanNext::Tasks(vec![self.model_task()?]))
            }
            SingleAgentStage::Model => {
                let response = TaskResp::<ModelResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| anyhow::anyhow!("expected ModelResponse"))?;
                self.handle_model_response(response.resp).await
            }
            SingleAgentStage::Tools { .. } => {
                let task_id = task_result.meta.id.clone();
                let response = TaskResp::<ToolResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| anyhow::anyhow!("expected ToolResponse"))?;
                self.handle_tool_response(task_id, response.resp).await
            }
            SingleAgentStage::Save => {
                let response = TaskResp::<SessionResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| anyhow::anyhow!("expected SessionResponse after save"))?;
                anyhow::ensure!(
                    matches!(response.resp, SessionResponse::Added { .. }),
                    "expected session add response"
                );
                self.session.finish_turn();
                self.emit(SingleAgentEvent::Completed {
                    turn_id: self.turn_id,
                    name: self.template.agent.name.clone(),
                    content: self.final_output.clone(),
                })
                .await?;
                Ok(PlanNext::End)
            }
        }
    }

    async fn abort(&mut self, _code: i32, error: String) {
        self.session.finish_turn();
        let _ = self
            .emit(SingleAgentEvent::Failed {
                turn_id: self.turn_id,
                name: self.template.agent.name.clone(),
                error,
            })
            .await;
    }
}

impl Drop for SingleAgentPlan {
    fn drop(&mut self) {
        self.session.finish_turn();
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

fn trim_messages_to_context(messages: &mut Vec<ChatCompletionRequestMessage>, context_size: usize) {
    let estimated_tokens = |message: &ChatCompletionRequestMessage| {
        serde_json::to_string(message)
            .map(|json| json.chars().count().div_ceil(4).max(1))
            .unwrap_or(1)
    };
    while messages.len() > 2 && messages.iter().map(estimated_tokens).sum::<usize>() > context_size
    {
        let remove_at = usize::from(matches!(
            messages.first(),
            Some(ChatCompletionRequestMessage::System(_))
        ));
        messages.remove(remove_at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::chat::{
        CreateChatCompletionResponse, FunctionCallStream, FunctionType,
    };

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
    fn context_limit_keeps_system_and_latest_user_message() {
        let mut messages = vec![
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: "system".into(),
                ..Default::default()
            }),
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: "old question".into(),
                ..Default::default()
            }),
            ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
                content: Some("old answer".into()),
                ..Default::default()
            }),
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: "latest question".into(),
                ..Default::default()
            }),
        ];

        trim_messages_to_context(&mut messages, 1);

        assert_eq!(messages.len(), 2);
        assert!(matches!(
            messages.first(),
            Some(ChatCompletionRequestMessage::System(_))
        ));
        assert!(matches!(
            messages.last(),
            Some(ChatCompletionRequestMessage::User(_))
        ));
    }

    #[tokio::test]
    async fn session_rejects_calls_before_binding() {
        let session = SingleAgentSession::new();
        let error = session.call("hello".to_string()).await.unwrap_err();
        assert!(error.to_string().contains("not bound"));
    }

    #[tokio::test]
    async fn no_tool_turn_streams_and_completes() {
        let session = SingleAgentSession::new();
        session.activate_turn().unwrap();
        let ctx = Ctx::null();
        let template = SingleAgentTemplate {
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
        };
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
            kinds.push(event.kind());
            if event.is_terminal() {
                break;
            }
        }
        assert_eq!(
            kinds,
            vec![
                "turn_started",
                "history_loaded",
                "model_output",
                "completed"
            ]
        );
    }
}
