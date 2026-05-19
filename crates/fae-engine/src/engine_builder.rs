use crate::engine::{AgentLoader, AgentsEngine};
use crate::runtime::plan_runtime::PlanRuntime;
use crate::runtime::task_runtime::{TaskRuntime, TaskRuntimeRef};
use crate::workspace::Workspace;
use crate::workspace_builder::WorkspaceBuilder;
use fae_agent::{Env, Environment};

impl AgentsEngine {
    pub async fn default() -> Self {
        AgentsEngine::new(TaskRuntime::new())
            .assemble_runtime(PlanRuntime::new()).await
    }
    pub async fn assemble_runtime(mut self, mut layer: impl Environment + Send + 'static) -> Self {
        let env = self.runtime.as_env();
        self.runtime = {
            layer.register_parent_env(env).await;
            TaskRuntimeRef::from(Env::new(layer))
        };
        self
    }
    pub fn set_workspaces<N: Into<String>>(&mut self, name: N, ws: Workspace) -> &mut Self {
        self.workspaces.insert(name.into(), ws);
        self
    }
    pub async fn build_workspace<N, L, E>(&mut self, name: N, loader: L, setting: E) -> Workspace
    where
        N: Into<String>,
        L: AgentLoader + Send + 'static,
        E: FnOnce(&mut WorkspaceBuilder),
    {
        let name = name.into();
        let mut workspace_builder =
            WorkspaceBuilder::new(name.clone(), loader, self.runtime.as_env());
        setting(&mut workspace_builder);
        let workspace = workspace_builder.build();
        self.set_workspaces(name.clone(), workspace.clone());
        workspace
    }
}
