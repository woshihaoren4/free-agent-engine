use std::sync::Arc;
use fae_agent::{Agent, AgentRef, Env, Session, Task, TaskResult};
use crate::task_executor::{TaskRuntime, TaskRuntimeRef};

pub struct RecallAgentRef{
    agent: AgentRef,
    score : f32,
}

#[async_trait::async_trait]
pub trait AgentLoader:Sync{
    async fn load(&self, agent_name: &str) -> anyhow::Result<AgentRef>;
    async fn recall(&self, task_desc: &str,limit: usize) -> anyhow::Result<Vec<RecallAgentRef>>;
}


#[derive(Clone)]
pub struct EngineEnvLayer{
    loader: Arc<dyn AgentLoader + Send + 'static>,
    env : Env,
}

pub struct AgentsEngine {
    layers: Vec<EngineEnvLayer>,
    executor: TaskRuntimeRef,
}

impl AgentsEngine {
    pub fn new<RT:Into<TaskRuntimeRef>>(executor: RT) -> Self {
        Self { layers: Vec::new(), executor:executor.into() }
    }
}