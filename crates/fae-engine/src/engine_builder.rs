use crate::engine::{AgentLoader, AgentsEngine};
use crate::workspace::{Workspace, WorkspaceBuilder};

impl AgentsEngine{
    pub fn set_workspaces<N:Into<String>>(&mut self,name:N,ws:Workspace)-> &mut Self {
        self.workspaces.insert(name.into(), ws);
        self
    }
    pub async fn build_workspace<N,L,E>(&mut self,name:N, loader: L,setting:E) -> &mut Self
    where
        N:Into<String>,
        L: AgentLoader+Send+'static,
        E: FnOnce(&mut WorkspaceBuilder),
    {
        let name = name.into();
        let mut workspace_builder = WorkspaceBuilder::new(name.clone(), loader, self.runtime.as_env());
        setting(&mut workspace_builder);
        self.set_workspaces(name, workspace_builder.build());
        self
    }
}
