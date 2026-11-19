use super::runtime::python::PythonActionRuntime;
use fae_agent::{FAEWorkflowMetadataLoader, SingleAgentPlanBuilder};
use fae_engine::{
    CompressionRuntime, DefaultTools, Engine, EngineBuilder, McpRuntime, ModelRuntime, PlanRuntime,
    SessionRuntime, SkillRuntime, ToolsRuntime, UserMemoryRuntime, WorkflowRuntime,
};

pub async fn build_engine(
    loader: FAEWorkflowMetadataLoader,
    mut agent_builder: SingleAgentPlanBuilder,
    timer: Option<fae_engine::TimerHandle>,
    plugins: std::sync::Arc<fae_agent::plugin::PluginManager>,
) -> Engine {
    let home = loader.home_dir().to_path_buf();
    let mut builder = EngineBuilder::new();
    builder.add_runtime(PlanRuntime::new());
    builder.add_runtime(WorkflowRuntime::with_metadata_loader(loader.clone()));
    builder.add_runtime(ModelRuntime::new().with_plugins(plugins.clone()));
    builder.add_runtime(CompressionRuntime::default());
    builder.add_runtime(SessionRuntime::with_host_dir(&home));
    builder.add_runtime(UserMemoryRuntime::with_host_dir(&home));
    builder.add_runtime(SkillRuntime::with_host_dir(&home));
    builder.add_runtime(McpRuntime::with_mcp_dir(home.join("mcp")));
    builder.add_runtime(PythonActionRuntime::default());
    if let Some(timer) = timer {
        builder.add_runtime(timer.client());
    }
    let mut tools = ToolsRuntime::new();
    tools.add_tool(Box::new(DefaultTools::with_host_dir(&home)));
    let tools = tools.with_plugins(plugins.clone());
    builder.add_runtime(tools);
    agent_builder.add_hook(fae_agent::plugin::adapter::PluginAdapter::new(
        plugins.clone(),
    ));
    builder.add_plan_builder(agent_builder);
    builder.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(loader));
    builder.build().await.with_plugins(plugins)
}
