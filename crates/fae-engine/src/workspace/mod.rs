mod workspace;
mod workspace_builder;
mod workspace_loader;
mod workspace_session;

use fae_agent::{AgentRef, Error};
use std::any::Any;
use wd_tools::PFErr;
pub use workspace::*;
pub use workspace_builder::*;
pub use workspace_loader::*;
pub use workspace_session::*;

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
        cfg: &mut Box<dyn Any + Send + Sync + 'static>,
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
        _name: &str,
        _prompt: &str,
        _cfg: &mut Box<dyn Any + Send + Sync + 'static>,
    ) -> anyhow::Result<AgentRef> {
        return Err(Error::NoSupport.into());
    }
    async fn exit(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
