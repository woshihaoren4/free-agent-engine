use std::collections::HashMap;
use std::sync::Arc;
use wd_tools::PFErr;
use fae_agent::{Agent, AgentRef, Env, Session, Task, TaskResult};
use crate::task_executor::{TaskRuntime, TaskRuntimeRef};
use crate::workspace::Workspace;

pub struct RecallAgentRef{
    agent: AgentRef,
    score : f32,
}

#[async_trait::async_trait]
pub trait AgentLoader:Sync{
    async fn load(&self, agent_id: &str) -> anyhow::Result<AgentRef>;
    async fn recall(&self, task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>>;
}
#[async_trait::async_trait]
impl AgentLoader for () {
    async fn load(&self, _agent_id: &str) -> anyhow::Result<AgentRef> {
        anyhow::anyhow!("NotFound").err()
    }
    async fn recall(&self, task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>> {
        anyhow::anyhow!("NotFound").err()
    }
}



pub struct AgentsEngine {
    pub workspaces: HashMap<String, Workspace>,
    pub runtime: TaskRuntimeRef,
}

impl AgentsEngine {
    pub fn new<RT:Into<TaskRuntimeRef>>(runtime: RT) -> Self {
        Self { workspaces: HashMap::new(), runtime: runtime.into() }
    }
    pub fn workspaces(&self,name:&str) -> Option<Workspace>{
        self.workspaces.get(name).cloned()
    }
}