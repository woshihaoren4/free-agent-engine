use crate::runtime::task_runtime::{TaskRuntime, TaskRuntimeRef};
use crate::workspace::Workspace;
use fae_agent::{Agent, AgentRef, Env, Task, TaskResult};
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use wd_tools::PFErr;

pub struct AgentsEngine {
    pub workspaces: HashMap<String, Workspace>,
    pub runtime: TaskRuntimeRef,
}

impl AgentsEngine {
    pub fn new<RT: Into<TaskRuntimeRef>>(runtime: RT) -> Self {
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
