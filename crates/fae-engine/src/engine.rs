use crate::runtime::task_runtime::{TaskRuntime, TaskRuntimeRef};
use crate::workspace::Workspace;
use fae_agent::{Agent, AgentRef, Env, Task, TaskResult};
use std::collections::HashMap;
use std::sync::Arc;
use wd_tools::PFErr;

pub struct RecallAgentRef {
    agent: AgentRef,
    score: f32,
}

#[async_trait::async_trait]
pub trait AgentLoader: Sync {
    async fn load(&self, agent_id: &str) -> anyhow::Result<AgentRef>;
    async fn recall(&self, task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>>;
    async fn exit(&self) -> anyhow::Result<()>;
}
#[async_trait::async_trait]
impl AgentLoader for () {
    async fn load(&self, _agent_id: &str) -> anyhow::Result<AgentRef> {
        anyhow::anyhow!("NotFound").err()
    }
    async fn recall(&self, task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>> {
        Ok(Vec::new())
    }
    async fn exit(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

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
