use std::sync::Arc;
use fae_agent::{Agent, AgentRef, Env, EnvWatch, EnvironmentWatch, Session};

#[async_trait::async_trait]
pub trait AgentLoader:Sync{
    async fn load_agent(&self, agent_name: &str) -> anyhow::Result<AgentRef>;
}

pub struct EngineEnvLayer{
    loader: Arc<dyn AgentLoader + Send + 'static>,
    env : Env,
    env_watch: EnvWatch,
}

pub struct AgentsEngine {
    layers: Vec<EngineEnvLayer>,
}