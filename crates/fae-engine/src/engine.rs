use crate::workspace::Workspace;
use fae_agent::{Agent, Env};
use std::collections::HashMap;

pub struct AgentsEngine {
    pub workspaces: HashMap<String, Workspace>,
    pub runtime: Env,
}

impl AgentsEngine {
    pub fn new<RT: Into<Env>>(runtime: RT) -> Self {
        Self {
            workspaces: HashMap::new(),
            runtime: runtime.into(),
        }
    }
    pub fn workspace(&self, name: &str) -> Option<Workspace> {
        self.workspaces.get(name).cloned()
    }
    pub async fn exit(&self) {
        for (_, workspace) in self.workspaces.iter() {
            workspace.exit().await;
        }
    }
}
