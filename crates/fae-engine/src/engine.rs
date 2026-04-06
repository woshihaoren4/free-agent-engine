use std::sync::Arc;
use fae_agent::{Agent, AgentRef, Env, EnvWatch, EnvironmentWatch, Session, Task, TaskResult};

#[async_trait::async_trait]
pub trait AgentLoader:Sync{
    async fn load_agent(&self, agent_name: &str) -> anyhow::Result<AgentRef>;
}

#[async_trait::async_trait]
pub trait TaskExecutor:Sync{
    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult>;
}

#[derive(Clone)]
pub struct EngineEnvLayer{
    loader: Arc<dyn AgentLoader + Send + 'static>,
    env : Env,
    env_watch: EnvWatch,
}

#[derive(Clone)]
pub struct AgentsEngine {
    layers: Vec<EngineEnvLayer>,
    executor: Arc<dyn TaskExecutor + Send + 'static>,
}