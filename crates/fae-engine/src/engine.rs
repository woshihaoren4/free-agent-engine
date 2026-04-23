use std::sync::Arc;
use fae_agent::{Agent, AgentRef, Env, Session, Task, TaskResult};
use crate::task_executor::{TaskRuntime, TaskRuntimeRef};

pub struct RecallAgentRef{
    agent: AgentRef,
    score : f32,
}

#[async_trait::async_trait]
pub trait AgentLoader:Sync{
    async fn load(&self, agent_id: &str) -> anyhow::Result<AgentRef>;
    async fn recall(&self, task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>>;
}


#[derive(Clone)]
pub struct EngineEnvLayer{
    loader: Arc<dyn AgentLoader + Send + 'static>,
    env : Env,
}

pub struct AgentsEngine {
    layers: Vec<EngineEnvLayer>,
    runtime: TaskRuntimeRef,
}

impl AgentsEngine {
    pub fn new<RT:Into<TaskRuntimeRef>>(runtime: RT) -> Self {
        Self { layers: Vec::new(), runtime: runtime.into() }
    }
}