use crate::engine::{AgentLoader, AgentsEngine};
use crate::workspace::{Workspace};
use crate::workspace_builder::WorkspaceBuilder;

impl AgentsEngine{
    pub fn set_workspaces<N:Into<String>>(&mut self,name:N,ws:Workspace)-> &mut Self {
        self.workspaces.insert(name.into(), ws);
        self
    }
    pub async fn build_workspace<N,L,E>(&mut self,name:N, loader: L,setting:E) -> Workspace
    where
        N:Into<String>,
        L: AgentLoader+Send+'static,
        E: FnOnce(&mut WorkspaceBuilder),
    {
        let name = name.into();
        let mut workspace_builder = WorkspaceBuilder::new(name.clone(), loader, self.runtime.as_env());
        setting(&mut workspace_builder);
        let workspace = workspace_builder.build();
        self.set_workspaces(name.clone(), workspace.clone());
        workspace
    }
}
