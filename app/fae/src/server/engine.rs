use fae_agent::{FAEWorkflowMetadataLoader, SingleAgentPlanBuilder};
use fae_engine::{
    CompressionRuntime, DefaultTools, Engine, EngineBuilder, McpRuntime, ModelRuntime, PlanRuntime,
    SessionRuntime, SkillRuntime, ToolsRuntime, UserMemoryRuntime, WorkflowRuntime,
};
use super::runtime::python::PythonActionRuntime;

pub async fn build_engine(loader: FAEWorkflowMetadataLoader, agent_builder: SingleAgentPlanBuilder) -> Engine {
    let home = loader.home_dir().to_path_buf();
    let mut builder = EngineBuilder::new();
    builder.add_runtime(PlanRuntime::new());
    builder.add_runtime(WorkflowRuntime::with_metadata_loader(loader.clone()));
    builder.add_runtime(ModelRuntime::new());
    builder.add_runtime(CompressionRuntime::default());
    builder.add_runtime(SessionRuntime::with_host_dir(&home));
    builder.add_runtime(UserMemoryRuntime::with_host_dir(&home));
    builder.add_runtime(SkillRuntime::with_host_dir(&home));
    builder.add_runtime(McpRuntime::with_mcp_dir(home.join("mcp")));
    builder.add_runtime(PythonActionRuntime::default());
    let mut tools = ToolsRuntime::new();
    tools.add_tool(Box::new(DefaultTools::with_host_dir(&home)));
    builder.add_runtime(tools);
    builder.add_plan_builder(agent_builder);
    builder.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(loader));
    builder.build().await
}
