//! A single-agent turn implemented directly by workflow nodes.
//!
//! ```text
//! main workflow
//!
//! +-------+   +---------------+   +--------------+   +---------------+
//! | start |-->| prepare_agent |-->| load_history |-->| initial_model |
//! +-------+   +---------------+   +--------------+   +-------+-------+
//!                                                               |
//!                                                        +------v------+
//!                                 +--------------------->|  tool_loop  |
//!                                 |                      +------+------+
//!                                 | has tool calls              | no calls
//!                                 |                             v
//!                         +-------+----------+             +----------+
//!                         | tool_iteration   |             | finalize |
//!                         | (child workflow) |             +----+-----+
//!                         +------------------+                  |
//!                                                               v
//!                                                        +--------------+
//!                                                        | save_history |
//!                                                        +------+-------+
//!                                                               |
//!                                                          +----v----+
//!                                                          |   end   |
//!                                                          +---------+
//!
//! tool_iteration child workflow
//!
//! +-------+   +---------------+   +------------+   +-----+
//! | start |-->| execute_tools |-->| call_model |-->| end |
//! +-------+   +---------------+   +------------+   +-----+
//! ```
//!
//! The example uses agent ID `fae` by default. Pass another ID as the first
//! command-line argument to load a different agent configuration.

use std::{
    collections::HashMap,
    io::{self, Write},
};

use async_openai::types::chat::{
    ChatCompletionMessageToolCalls, ChatCompletionRequestAssistantMessage,
    ChatCompletionRequestAssistantMessageContent, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessage, ChatCompletionRequestToolMessage,
    ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage, ChatCompletionTool,
    ChatCompletionTools, CreateChatCompletionRequest, FunctionObject,
};
use fae_agent::{
    Ctx, FAEWorkflowMetadataLoader, McpQuery, McpRequest, McpResponse, McpToolInfo, ModelResponse,
    Session, SessionEventData, SessionInput, SessionMessage, SessionMessageRole, SessionOutput,
    SessionRequest, SessionResponse, SingleAgentConfig, SingleAgentPlanBuilder, SingleAgentSource,
    SkillInfo, TaskMeta, TaskReq, TaskType, ToolRequest, ToolRespItem, ToolResponse, Tools,
    WorkflowAction, WorkflowCondition, WorkflowEnv, WorkflowMetadata, WorkflowMetadataBuilder,
};
use fae_engine::EngineBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const WORKFLOW_ID: &str = "single-agent-workflow";
const TOOL_ITERATION_WORKFLOW_ID: &str = "single-agent-tool-iteration";
const DEFAULT_AGENT_ID: &str = "fae";
const AGENT_WORKFLOW_TOOL_CHANNEL: &str = "agent_workflow";
const WORKFLOW_MAX_TOOL_ITERATIONS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ToolRoute {
    Tool { tool_name: String },
    Mcp { server: String, tool_name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentSetup {
    config: SingleAgentConfig,
    prompt: String,
    tool_definitions: Vec<ChatCompletionTools>,
    tool_routes: HashMap<String, ToolRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentState {
    setup: AgentSetup,
    messages: Vec<ChatCompletionRequestMessage>,
    tool_calls: Vec<ChatCompletionMessageToolCalls>,
    final_output: String,
    tool_iterations: usize,
    has_tool_calls: bool,
}

#[derive(Debug, Deserialize)]
struct InitialModelInput {
    setup: AgentSetup,
    history: SessionResponse,
    input: String,
}

#[derive(Debug, Default)]
struct AgentWorkflowTools;

#[async_trait::async_trait]
impl Tools for AgentWorkflowTools {
    fn channel(&self) -> &str {
        AGENT_WORKFLOW_TOOL_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        anyhow::bail!("internal workflow tool `{tool_name}` is not exposed to the model")
    }

    async fn exec(&self, ctx: &Ctx, request: ToolRequest) -> anyhow::Result<ToolResponse> {
        let action = request
            .tool_name
            .strip_prefix(&format!("{AGENT_WORKFLOW_TOOL_CHANNEL}__"))
            .ok_or_else(|| {
                anyhow::anyhow!("invalid agent workflow tool `{}`", request.tool_name)
            })?;
        let payload: Value = serde_json::from_str(&request.arguments)?;
        let rt = ctx.get_engine().rt();

        let output = match action {
            "prepare_agent" => {
                let agent_id = required_string(&payload, "agent_id")?;
                serde_json::to_value(prepare_agent(&rt, agent_id).await?)?
            }
            "initial_model" => {
                let input: InitialModelInput = serde_json::from_value(payload)?;
                serde_json::to_value(initial_model(&rt, ctx.clone(), input).await?)?
            }
            "execute_tools" => {
                let state: AgentState = serde_json::from_value(payload)?;
                serde_json::to_value(execute_tools(&rt, ctx.clone(), state).await?)?
            }
            "call_model" => {
                let state: AgentState = serde_json::from_value(payload)?;
                serde_json::to_value(call_model(&rt, ctx.clone(), state).await?)?
            }
            "finalize" => {
                let state: AgentState = serde_json::from_value(payload)?;
                anyhow::ensure!(
                    !state.has_tool_calls,
                    "cannot finalize while model tool calls are pending"
                );
                serde_json::to_value(state)?
            }
            _ => anyhow::bail!("unsupported agent workflow action `{action}`"),
        };

        Ok(ToolResponse::with_result(serde_json::to_string(&output)?))
    }
}

fn build_single_agent_workflow() -> anyhow::Result<WorkflowMetadata> {
    let mut builder = WorkflowMetadataBuilder::new(WORKFLOW_ID);
    builder.start("start", "prepare_agent")?;
    builder.execute(
        "prepare_agent",
        agent_step("prepare_agent", json!({ "agent_id": "{$input.agent_id}" })),
        "load_history",
    )?;
    builder.execute(
        "load_history",
        WorkflowAction::Session {
            request: SessionRequest::Query {
                user: "{$prepare_agent.config.agent.user_id}".to_string(),
                session_id: "{$prepare_agent.config.agent.session_id}".to_string(),
                limit: None,
                offset: None,
            },
        },
        "initial_model",
    )?;
    builder.execute(
        "initial_model",
        agent_step(
            "initial_model",
            json!({
                "setup": "{$prepare_agent}",
                "history": "{$load_history}",
                "input": "{$input.message}"
            }),
        ),
        "tool_loop",
    )?;
    builder.loop_node(
        "tool_loop",
        WorkflowCondition::Truthy {
            value: json!("{$last.has_tool_calls}"),
        },
        "tool_iteration",
        "finalize",
        WORKFLOW_MAX_TOOL_ITERATIONS,
    )?;
    builder.execute(
        "tool_iteration",
        WorkflowAction::Workflow {
            workflow_id: TOOL_ITERATION_WORKFLOW_ID.to_string(),
            input: json!("{$last}"),
        },
        "tool_loop",
    )?;
    builder.execute(
        "finalize",
        agent_step("finalize", json!("{$last}")),
        "save_history",
    )?;
    builder.execute(
        "save_history",
        WorkflowAction::Session {
            request: SessionRequest::Add {
                user: "{$finalize.setup.config.agent.user_id}".to_string(),
                session_id: "{$finalize.setup.config.agent.session_id}".to_string(),
                messages: vec![
                    SessionMessage::user("{$input.message}"),
                    SessionMessage::assistant("{$finalize.final_output}"),
                ],
            },
        },
        "end",
    )?;
    builder.end("end", Some(json!("{$finalize.final_output}")))?;
    builder.build()
}

fn build_tool_iteration_workflow() -> anyhow::Result<WorkflowMetadata> {
    let mut builder = WorkflowMetadataBuilder::new(TOOL_ITERATION_WORKFLOW_ID);
    builder.start("start", "execute_tools")?;
    builder.execute(
        "execute_tools",
        agent_step("execute_tools", json!("{$input}")),
        "call_model",
    )?;
    builder.execute(
        "call_model",
        agent_step("call_model", json!("{$execute_tools}")),
        "end",
    )?;
    builder.end("end", Some(json!("{$call_model}")))?;
    builder.build()
}

fn agent_step(action: &str, payload: Value) -> WorkflowAction {
    WorkflowAction::Tool {
        tool_name: format!("{AGENT_WORKFLOW_TOOL_CHANNEL}__{action}"),
        arguments: payload,
    }
}

async fn prepare_agent(rt: &fae_agent::RT, agent_id: &str) -> anyhow::Result<AgentSetup> {
    let source = SingleAgentSource::AgentId(agent_id.to_string());
    let (config, mut prompt) = SingleAgentPlanBuilder::new().load_config(&source).await?;

    let mut skills = Vec::new();
    for query in &config.skills {
        skills.extend(
            rt.select::<_, Vec<SkillInfo>>(TaskType::Skill, query.clone())
                .await?,
        );
    }
    if !skills.is_empty() {
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
    }

    let mut tool_definitions = Vec::new();
    let mut tool_routes = HashMap::new();
    for tool_name in &config.tools {
        let description = rt
            .select::<_, Value>(TaskType::Tool, tool_name.clone())
            .await?;
        let function: FunctionObject = serde_json::from_value(description)?;
        anyhow::ensure!(
            tool_routes
                .insert(
                    function.name.clone(),
                    ToolRoute::Tool {
                        tool_name: tool_name.clone()
                    }
                )
                .is_none(),
            "duplicate model tool name `{}`",
            function.name
        );
        tool_definitions.push(ChatCompletionTools::Function(ChatCompletionTool {
            function,
        }));
    }

    for server in &config.mcp_servers {
        let tools = rt
            .select::<_, Vec<McpToolInfo>>(TaskType::Mcp, McpQuery::new(server))
            .await?;
        for tool in tools {
            let model_name = tool.model_name();
            anyhow::ensure!(
                tool_routes
                    .insert(
                        model_name.clone(),
                        ToolRoute::Mcp {
                            server: tool.server,
                            tool_name: tool.name,
                        }
                    )
                    .is_none(),
                "duplicate model tool name `{model_name}`"
            );
            tool_definitions.push(ChatCompletionTools::Function(ChatCompletionTool {
                function: FunctionObject {
                    name: model_name,
                    description: (!tool.description.is_empty()).then_some(tool.description),
                    parameters: Some(tool.input_schema),
                    strict: None,
                },
            }));
        }
    }

    Ok(AgentSetup {
        config,
        prompt,
        tool_definitions,
        tool_routes,
    })
}

async fn initial_model(
    rt: &fae_agent::RT,
    ctx: fae_agent::Ctx,
    input: InitialModelInput,
) -> anyhow::Result<AgentState> {
    let SessionResponse::History {
        messages: history, ..
    } = input.history
    else {
        anyhow::bail!("load_history did not return session history");
    };

    let mut messages = Vec::new();
    if !input.setup.prompt.is_empty() {
        messages.push(ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                content: input.setup.prompt.clone().into(),
                ..Default::default()
            },
        ));
    }
    let history_limit = input.setup.config.model.history_turns.saturating_mul(2);
    for message in &history[history.len().saturating_sub(history_limit)..] {
        messages.push(session_message_to_chat(message));
    }
    messages.push(ChatCompletionRequestMessage::User(
        ChatCompletionRequestUserMessage {
            content: input.input.clone().into(),
            ..Default::default()
        },
    ));
    trim_messages_to_context(
        &mut messages,
        input.setup.config.model.trigger_compression_size,
    );

    let state = AgentState {
        setup: input.setup,
        messages,
        tool_calls: Vec::new(),
        final_output: String::new(),
        tool_iterations: 0,
        has_tool_calls: false,
    };
    call_model(rt, ctx, state).await
}

async fn call_model(
    rt: &fae_agent::RT,
    ctx: fae_agent::Ctx,
    mut state: AgentState,
) -> anyhow::Result<AgentState> {
    let model = &state.setup.config.model;
    let request = CreateChatCompletionRequest {
        model: model.model.clone(),
        messages: state.messages.clone(),
        stream: Some(false),
        max_completion_tokens: model.max_completion_tokens,
        temperature: model.temperature,
        tools: (!state.setup.tool_definitions.is_empty())
            .then(|| state.setup.tool_definitions.clone()),
        safety_identifier: Some(state.setup.config.agent.user_id.clone()),
        ..Default::default()
    };
    let response = rt
        .exec::<_, ModelResponse>(TaskReq {
            ctx,
            meta: TaskMeta {
                ty: TaskType::Model,
                ..Default::default()
            },
            req: request,
        })
        .await?
        .resp
        .into_completed()
        .ok_or_else(|| anyhow::anyhow!("agent workflow expected a non-streaming model response"))?;
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("model returned no choices"))?;
    let content = choice.message.content.unwrap_or_default();
    let tool_calls = choice.message.tool_calls.unwrap_or_default();

    state.messages.push(ChatCompletionRequestMessage::Assistant(
        ChatCompletionRequestAssistantMessage {
            content: (!content.is_empty()).then_some(
                ChatCompletionRequestAssistantMessageContent::Text(content.clone()),
            ),
            tool_calls: (!tool_calls.is_empty()).then(|| tool_calls.clone()),
            ..Default::default()
        },
    ));
    state.has_tool_calls = !tool_calls.is_empty();
    state.tool_calls = tool_calls;
    if !state.has_tool_calls {
        state.final_output = content.clone();
    }
    Ok(state)
}

async fn execute_tools(
    rt: &fae_agent::RT,
    ctx: fae_agent::Ctx,
    mut state: AgentState,
) -> anyhow::Result<AgentState> {
    state.tool_iterations += 1;
    anyhow::ensure!(
        state.tool_iterations <= state.setup.config.model.max_tool_iterations,
        "model exceeded max_tool_iterations ({})",
        state.setup.config.model.max_tool_iterations
    );

    for call in std::mem::take(&mut state.tool_calls) {
        let ChatCompletionMessageToolCalls::Function(call) = call else {
            anyhow::bail!("custom tool calls are not supported");
        };
        let route = state
            .setup
            .tool_routes
            .get(&call.function.name)
            .ok_or_else(|| {
                anyhow::anyhow!("model requested unavailable tool `{}`", call.function.name)
            })?;
        let output = match route {
            ToolRoute::Tool { tool_name } => {
                let mut response = rt
                    .exec::<_, ToolResponse>(TaskReq {
                        ctx: ctx.clone(),
                        meta: TaskMeta {
                            ty: TaskType::Tool,
                            ..Default::default()
                        },
                        req: ToolRequest::new(tool_name.clone(), call.function.arguments.clone()),
                    })
                    .await?
                    .resp;
                loop {
                    match response.next().await? {
                        ToolRespItem::Streaming(_) => {}
                        ToolRespItem::Completed(output) => break output,
                    }
                }
            }
            ToolRoute::Mcp { server, tool_name } => {
                rt.exec::<_, McpResponse>(TaskReq {
                    ctx: ctx.clone(),
                    meta: TaskMeta {
                        ty: TaskType::Mcp,
                        ..Default::default()
                    },
                    req: McpRequest::new(
                        server.clone(),
                        tool_name.clone(),
                        call.function.arguments.clone(),
                    ),
                })
                .await?
                .resp
                .output
            }
        };
        state.messages.push(ChatCompletionRequestMessage::Tool(
            ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(output),
                tool_call_id: call.id,
            },
        ));
    }
    state.has_tool_calls = false;
    Ok(state)
}

fn session_message_to_chat(message: &SessionMessage) -> ChatCompletionRequestMessage {
    match message.role {
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
        SessionMessageRole::Summary => {
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: format!("Compressed conversation context:\n{}", message.content).into(),
                ..Default::default()
            })
        }
    }
}

fn trim_messages_to_context(
    messages: &mut Vec<ChatCompletionRequestMessage>,
    trigger_compression_size: usize,
) {
    let estimated_tokens = |message: &ChatCompletionRequestMessage| {
        serde_json::to_string(message)
            .map(|json| json.chars().count().div_ceil(4).max(1))
            .unwrap_or(1)
    };
    while messages.len() > 2
        && messages.iter().map(estimated_tokens).sum::<usize>() > trigger_compression_size
    {
        let remove_at = usize::from(matches!(
            messages.first(),
            Some(ChatCompletionRequestMessage::System(_))
        ));
        messages.remove(remove_at);
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> anyhow::Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string field `{field}`"))
}

async fn build_engine(loader: FAEWorkflowMetadataLoader) -> fae_engine::Engine {
    let mut builder = EngineBuilder::new();
    builder.add_runtime(fae_engine::WorkflowRuntime::with_metadata_loader(
        loader.clone(),
    ));
    builder.add_runtime(fae_engine::ModelRuntime::new());
    builder.add_runtime(fae_engine::SessionRuntime::new());
    builder.add_runtime(fae_engine::SkillRuntime::new());
    builder.add_runtime(fae_engine::McpRuntime::new());

    let mut tools = fae_engine::ToolsRuntime::new();
    tools.add_tool(Box::new(fae_engine::DefaultTools::default()));
    tools.add_tool(Box::new(AgentWorkflowTools));
    builder.add_runtime(tools);

    builder.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(loader));
    builder.build().await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let agent_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_AGENT_ID.to_string());
    let loader = FAEWorkflowMetadataLoader::new();
    loader.add(build_single_agent_workflow()?)?;
    loader.add(build_tool_iteration_workflow()?)?;
    let engine = build_engine(loader).await;

    println!("Enter a message. Use /exit or /quit to stop.");
    while let Some(input) = read_user_input()? {
        let (env, session) = WorkflowEnv::new(
            WORKFLOW_ID,
            json!({
                "agent_id": &agent_id,
                "message": input
            }),
        );

        let execution = engine.launch(env).await?;
        print_session(&session).await?;
        let output = execution.result::<Value>().await?;
        println!("\nassistant> {}", display_value(&output));
    }
    engine.exit().await?;
    Ok(())
}

fn read_user_input() -> anyhow::Result<Option<String>> {
    loop {
        print!("user> ");
        io::stdout().flush()?;

        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            println!();
            return Ok(None);
        }
        let input = input.trim();
        if matches!(input, "/exit" | "/quit") {
            return Ok(None);
        }
        if !input.is_empty() {
            return Ok(Some(input.to_string()));
        }
    }
}

async fn print_session(session: &impl Session<SessionInput, SessionOutput>) -> anyhow::Result<()> {
    while let Some(event) = session.answer().await? {
        let terminal = event.is_terminal();
        match event.event_data()? {
            SessionEventData::NodeCompleted { .. } => {
                println!("completed> {}", event.node_id.as_deref().unwrap_or("-"));
            }
            SessionEventData::Failed { error } => {
                eprintln!("workflow failed> {error}");
            }
            _ => {}
        }
        if terminal {
            break;
        }
    }
    Ok(())
}

fn display_value(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::WorkflowNode;

    #[test]
    fn workflow_expands_single_agent_stages_without_single_agent_action() -> anyhow::Result<()> {
        let workflow = build_single_agent_workflow()?;

        assert!(workflow.nodes.contains_key("load_history"));
        assert!(workflow.nodes.contains_key("initial_model"));
        assert!(workflow.nodes.contains_key("tool_loop"));
        assert!(workflow.nodes.contains_key("save_history"));
        assert!(workflow.nodes.values().all(|node| match node {
            WorkflowNode::Execute { action, .. } => matches!(
                action,
                WorkflowAction::Session { .. }
                    | WorkflowAction::Workflow { .. }
                    | WorkflowAction::Tool { .. }
            ),
            _ => true,
        }));
        Ok(())
    }

    #[test]
    fn tool_iteration_is_an_explicit_child_workflow() -> anyhow::Result<()> {
        let workflow = build_tool_iteration_workflow()?;

        assert!(workflow.nodes.contains_key("execute_tools"));
        assert!(workflow.nodes.contains_key("call_model"));
        Ok(())
    }
}
