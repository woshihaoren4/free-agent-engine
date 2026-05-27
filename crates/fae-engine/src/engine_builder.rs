use crate::WorkspaceBuilder;
use crate::engine::AgentsEngine;
use crate::runtime::plan_runtime::PlanRuntime;
use crate::runtime::task_runtime::{TaskRuntime, TaskRuntimeRef};
use crate::workspace::Workspace;
use fae_agent::{Env, Environment};

impl AgentsEngine {
    pub async fn default() -> Self {
        AgentsEngine::new(TaskRuntime::default())
            .assemble_runtime(PlanRuntime::new())
            .await
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
    pub async fn build_workspace<N, E>(&mut self, name: N, setting: E) -> Workspace
    where
        N: Into<String>,
        E: FnOnce(&mut WorkspaceBuilder),
    {
        let name = name.into();
        let mut workspace_builder = WorkspaceBuilder::new(name.clone(), self.runtime.as_env()).default_init().await;
        setting(&mut workspace_builder);
        let workspace = workspace_builder.build();
        self.set_workspaces(name.clone(), workspace.clone());
        workspace
    }
}
