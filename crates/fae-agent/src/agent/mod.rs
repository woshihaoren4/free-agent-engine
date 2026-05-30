pub mod single_agent;
pub use single_agent::*;

use crate::session::Session;
use crate::{Command, Env, EnvEvent, Memory, MemoryMessageExt, SessionMetadata};
use std::ops::Deref;
use std::sync::Arc;



/// 智能体 trait，定义智能体的核心接口
#[async_trait::async_trait]
pub trait Agent: Sync {
    /// 智能体ID
    fn id(&self) -> String;

    /// 智能体描述
    fn desc(&self) -> String {
        String::new()
    }
    
    /// 获取memory
    async fn on_memory(&self) -> Box<dyn Memory+Send+'static>;
    
    /// 
    

    /// 处理环境事件
    async fn on_env(&self, env: Env, event: EnvEvent) -> anyhow::Result<()>;

    /// 处理会话请求
    async fn on_session(
        &self,
        env: Env,
        meta: SessionMetadata,
    ) -> anyhow::Result<Box<dyn Session + Send + 'static>>;

    /// 处理命令
    async fn on_command(&self, env: Env, cmd: Command) -> anyhow::Result<()>;

    /// 退出
    async fn exit(&self) {}
}

#[derive(Clone)]
pub struct AgentRef(Arc<dyn Agent + Send + 'static>);
impl Deref for AgentRef {
    type Target = Arc<dyn Agent + Send + 'static>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<T> From<T> for AgentRef
where
    T: Agent + Send + 'static,
{
    fn from(agent: T) -> Self {
        Self(Arc::new(agent))
    }
}
