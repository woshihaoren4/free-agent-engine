mod file_single_agent_ctl;
mod workspace;
mod workspace_builder;
mod workspace_fn;
mod workspace_runtime;
mod workspace_session;

use fae_agent::{AgentConfig, AgentRef, Error};
pub use file_single_agent_ctl::*;
use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;
use wd_tools::PFErr;
pub use workspace::*;
pub use workspace_builder::*;
pub use workspace_runtime::*;

pub struct RecallAgentRef {
    agent: AgentRef,
    score: f32,
}

#[async_trait::async_trait]
pub trait AgentCtl: Debug + Sync {
    fn id(&self) -> &str {
        "default"
    }
    async fn load(&self, agent_id: &str) -> anyhow::Result<AgentRef>;
    async fn recall(&self, task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>>;
    async fn list(&self, limit: usize, offset: usize) -> anyhow::Result<Vec<AgentRef>>;
    async fn create(
        &self,
        agent_ctl_id: &str,
        agent_id: &str,
        cfg: Box<dyn AgentConfig + Send + 'static>,
    ) -> anyhow::Result<AgentRef>;
    async fn exit(&self) -> anyhow::Result<()>;
}

/// ---------- 默认实现 ----------

#[async_trait::async_trait]
impl AgentCtl for () {
    async fn load(&self, _agent_id: &str) -> anyhow::Result<AgentRef> {
        anyhow::anyhow!("NotFound").err()
    }
    async fn recall(&self, _task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>> {
        Ok(Vec::new())
    }
    async fn list(&self, _limit: usize, _offset: usize) -> anyhow::Result<Vec<AgentRef>> {
        Err(Error::NoSupport.into())
    }
    async fn create(
        &self,
        _agent_ctl_id: &str,
        _agent_id: &str,
        _cfg: Box<dyn AgentConfig + Send + 'static>,
    ) -> anyhow::Result<AgentRef> {
        Err(Error::NoSupport.into())
    }
    async fn exit(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// ---------- 扩展实现 ----------

// 内部 trait，用于安全地把 dyn AgentConfig 转换成 dyn Any。
pub(crate) trait ErasedAgentConfig {
    fn as_any(&self) -> &dyn Any;

    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T> ErasedAgentConfig for T
where
    T: AgentConfig + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[derive(Debug)]
pub struct AgentLoaderLayer<T> {
    o: Arc<dyn AgentCtl + Send + 'static>,
    n: T,
}

impl<T> AgentLoaderLayer<T> {
    pub fn new(o: Arc<dyn AgentCtl + Send + 'static>, t: T) -> Self {
        AgentLoaderLayer { o, n: t }
    }
}

#[async_trait::async_trait]
impl<T> AgentCtl for AgentLoaderLayer<T>
where
    T: AgentCtl + Send + 'static,
{
    async fn load(&self, agent_id: &str) -> anyhow::Result<AgentRef> {
        match self.n.load(agent_id).await {
            Ok(o) => Ok(o),
            Err(e) => {
                if let Some(Error::NoSupport) = e.downcast_ref::<Error>() {
                    self.o.load(agent_id).await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn recall(&self, task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>> {
        match self.n.recall(task_desc).await {
            Ok(o) => Ok(o),
            Err(e) => {
                if let Some(Error::NoSupport) = e.downcast_ref::<Error>() {
                    self.o.recall(task_desc).await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn list(&self, limit: usize, offset: usize) -> anyhow::Result<Vec<AgentRef>> {
        match self.n.list(limit, offset).await {
            Ok(o) => Ok(o),
            Err(e) => {
                if let Some(Error::NoSupport) = e.downcast_ref::<Error>() {
                    self.o.list(limit, offset).await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn create(
        &self,
        agent_ctl_id: &str,
        agent_id: &str,
        cfg: Box<dyn AgentConfig + Send + 'static>,
    ) -> anyhow::Result<AgentRef> {
        if agent_ctl_id != self.n.id() {
            self.o.create(agent_ctl_id, agent_id, cfg).await
        } else {
            self.n.create(agent_ctl_id, agent_id, cfg).await
        }
    }

    async fn exit(&self) -> anyhow::Result<()> {
        if let Err(err) = self.n.exit().await {
            wd_log::log_error_ln!("[AgentLoaderLayer] exit new loader error: {:?}", err);
        }
        self.o.exit().await
    }
}
