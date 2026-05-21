mod workspace;
mod workspace_builder;
mod workspace_loader;

use fae_agent::AgentRef;
use std::any::Any;
use wd_tools::PFErr;
pub use workspace::*;
pub use workspace_builder::*;
pub use workspace_loader::*;

pub struct RecallAgentRef {
    agent: AgentRef,
    score: f32,
}

#[async_trait::async_trait]
pub trait AgentLoader: Sync {
    async fn load(&self, agent_id: &str) -> anyhow::Result<AgentRef>;
    async fn recall(&self, task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>>;
    async fn create(
        &self,
        name: &str,
        prompt: &str,
        cfg: Box<dyn Any + Send + Sync + 'static>,
    ) -> anyhow::Result<AgentRef>;
    async fn exit(&self) -> anyhow::Result<()>;
}
#[async_trait::async_trait]
impl AgentLoader for () {
    async fn load(&self, _agent_id: &str) -> anyhow::Result<AgentRef> {
        anyhow::anyhow!("NotFound").err()
    }
    async fn recall(&self, _task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>> {
        Ok(Vec::new())
    }
    async fn create(
        &self,
        name: &str,
        _prompt: &str,
        _cfg: Box<dyn Any + Send + Sync + 'static>,
    ) -> anyhow::Result<AgentRef> {
        anyhow::anyhow!("CreateAgentNotSupported, name: {}", name).err()
    }
    async fn exit(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
