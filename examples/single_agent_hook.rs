use std::sync::Arc;

use async_openai::types::chat::CreateChatCompletionRequest;
use fae_agent::{
    ModelResponse, Session, SessionEventData, SingleAgentEnv, SingleAgentHook,
    SingleAgentHookBuilder, SingleAgentHookContext, SingleAgentPlanBuilder, ToolRequest,
};
use fae_engine::{
    CompressionRuntime, DefaultTools, EngineBuilder, McpRuntime, ModelRuntime, PlanRuntime,
    SessionRuntime, SkillRuntime, ToolsRuntime, UserMemoryRuntime, WorkflowRuntime,
};

#[derive(Debug)]
struct TraceHookBuilder {
    prompt_suffix: String,
}

#[async_trait::async_trait]
impl SingleAgentHookBuilder for TraceHookBuilder {
    async fn build(&self) -> Arc<dyn SingleAgentHook> {
        Arc::new(TraceHook {
            prompt_suffix: self.prompt_suffix.clone(),
        })
    }
}

#[derive(Debug)]
struct TraceHook {
    prompt_suffix: String,
}

#[async_trait::async_trait]
impl SingleAgentHook for TraceHook {
    async fn on_prompt(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        mut prompt: String,
    ) -> anyhow::Result<String> {
        println!(
            "[hook] prompt: agent={}, plan={}, turn={}",
            ctx.agent.name, ctx.plan_id, ctx.turn_id
        );
        prompt.push_str("\n<hook_instruction>\n");
        prompt.push_str(&self.prompt_suffix);
        prompt.push_str("\n</hook_instruction>\n");
        Ok(prompt)
    }

    async fn on_model_req(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        request: CreateChatCompletionRequest,
    ) -> anyhow::Result<CreateChatCompletionRequest> {
        println!(
            "[hook] model request: turn={}, messages={}",
            ctx.turn_id,
            request.messages.len()
        );
        Ok(request)
    }

    async fn on_model_resp(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        response: ModelResponse,
    ) -> anyhow::Result<ModelResponse> {
        println!(
            "[hook] model response: turn={}, streaming={}",
            ctx.turn_id,
            response.is_streaming()
        );
        Ok(response)
    }

    async fn on_tools_req(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        request: ToolRequest,
    ) -> anyhow::Result<ToolRequest> {
        println!(
            "[hook] tool request: turn={}, tool={}",
            ctx.turn_id,
            request.get_tool_name()
        );
        Ok(request)
    }
}

async fn build_engine() -> fae_engine::Engine {
    let mut builder = EngineBuilder::new();
    let workflow_loader = fae_agent::FAEWorkflowMetadataLoader::new();

    builder.add_runtime(PlanRuntime::new());
    builder.add_runtime(WorkflowRuntime::with_metadata_loader(
        workflow_loader.clone(),
    ));
    builder.add_runtime(ModelRuntime::new());
    builder.add_runtime(CompressionRuntime::default());
    builder.add_runtime(SessionRuntime::new());
    builder.add_runtime(UserMemoryRuntime::new());
    builder.add_runtime(SkillRuntime::new());
    builder.add_runtime(McpRuntime::new());

    let mut tools_runtime = ToolsRuntime::new();
    tools_runtime.add_tool(Box::new(DefaultTools::default()));
    builder.add_runtime(tools_runtime);

    let mut agent_builder = SingleAgentPlanBuilder::new();
    agent_builder.add_hook(TraceHookBuilder {
        prompt_suffix: "Keep the final answer concise.".to_string(),
    });
    builder.add_plan_builder(agent_builder);
    builder.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(workflow_loader));

    builder.build().await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let agent_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "workspace-assistant".to_string());
    let input = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "Summarize the current workspace.".to_string());

    let engine = build_engine().await;
    let (env, session) = SingleAgentEnv::from_agent_id(agent_id, input);
    let execution = engine.launch(env).await?;

    while let Some(event) = session.answer().await? {
        match event.event_data()? {
            SessionEventData::ModelReasoning { content } => print!("{content}"),
            SessionEventData::ModelOutput { content } => print!("{content}"),
            SessionEventData::Completed { .. } => {
                println!();
                break;
            }
            SessionEventData::Failed { error } => {
                anyhow::bail!("single agent failed: {error}");
            }
            _ => {}
        }
    }

    execution.result::<()>().await?;
    engine.exit().await?;
    Ok(())
}
