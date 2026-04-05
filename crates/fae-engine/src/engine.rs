use fae_agent::{Agent, Session};

#[async_trait::async_trait]
pub trait AgentLoader{
    async fn load_agent(&self, agent_id: &str) -> anyhow::Result<Box<dyn Agent + Send + Sync + 'static>>;
}


#[async_trait::async_trait]
pub trait Gateway{
    async fn new_session(&self) -> anyhow::Result<Box<dyn Session + Send + Sync + 'static>>;
}

pub struct AgentsEngine {

}
