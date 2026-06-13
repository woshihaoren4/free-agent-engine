use crate::agent_runtime::DefaultAgentTaskStore;
use crate::engine::AgentsEngine;
use crate::runtime::exec_runtime::ExecRuntime;
use crate::runtime::plan_runtime::PlanRuntime;
use crate::workspace::Workspace;
use crate::{
    AgentRuntime, CronRuntime, McpExecutor, ModelOpenAIApiExecutor, SingleAgentCtlFromFile,
    SkillsExecutor, ToolExecutor, ToolSetImplMap, WorkspaceBuilder, WorkspaceRuntime,
};
use fae_agent::{Environment, TaskType};

impl AgentsEngine {
    pub async fn default() -> Self {
        let mut engine = AgentsEngine::new(
            ExecRuntime::new()
                .register_executor(TaskType::Model, ModelOpenAIApiExecutor::default())
                .register_executor_ext(
                    TaskType::Tool,
                    ToolExecutor::from(
                        ToolSetImplMap::new()
                            .add_tool(crate::tools::ExecuteCommand::default())
                            .add_tool(crate::tools::SendHttpRequest)
                            .add_tool(crate::tools::ReadFile)
                            .add_tool(crate::tools::WriteFile::default())
                            .add_tool(crate::tools::ListDirectory)
                            .add_tool(crate::tools::ApplyPatch::default())
                            .add_tool(crate::tools::ExecutePython)
                            .add_tool(crate::tools::TodoWrite::default())
                            .add_tool(crate::tools::ArkWebSearch::default()),
                    ),
                )
                .register_executor(TaskType::Skill, SkillsExecutor::default())
                .register_executor(TaskType::Mcp, McpExecutor::default())
                .into_self(),
        )
        .assemble_runtime(CronRuntime::new())
        .await
        .assemble_runtime(AgentRuntime::new(DefaultAgentTaskStore::default()))
        .await
        .assemble_runtime(PlanRuntime::new())
        .await;
        // add default workspace main
        let mut builder = WorkspaceBuilder::new("main", engine.runtime.clone());
        builder.set_loader(SingleAgentCtlFromFile::with_workspace(builder.get_name()));
        let loader = builder.get_loader();
        builder
            .add_env_layer(WorkspaceRuntime::new(
                builder.get_name().to_string(),
                loader,
            ))
            .await;
        engine.set_workspaces(builder.get_name().to_string(), builder.build());
        engine
    }
    pub async fn assemble_runtime(mut self, mut layer: impl Environment + Send + 'static) -> Self {
        let env = self.runtime.clone();
        self.runtime = {
            layer.register_parent_env(env).await;
            layer.into()
        };
        self
    }
    pub fn set_workspaces<N: Into<String>>(&mut self, name: N, ws: Workspace) -> &mut Self {
        self.workspaces.insert(name.into(), ws);
        self
    }
    pub async fn build_workspace<N, E>(&mut self, name: N, setting: E) -> Workspace
    where
        N: Into<String>,
        E: FnOnce(&mut WorkspaceBuilder),
    {
        let name = name.into();
        let mut workspace_builder = WorkspaceBuilder::new(name.clone(), self.runtime.clone());
        setting(&mut workspace_builder);
        let workspace = workspace_builder.build();
        self.set_workspaces(name.clone(), workspace.clone());
        workspace
    }
}
