use std::sync::Arc;
use serde::{Deserialize, Serialize};
use crate::memory::Memory;
use crate::{Command, Env, EnvEvent, Session, SessionInfo, SessionMetaManager};

#[derive(Default, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SingleAgentSessionConfig {
    /// 基础路径，用于存储代理的配置文件和状态文件
    pub base_path: String,
    /// 模型名称,如："gpt-3.5-turbo"
    pub model_name: String,
    /// 模型渠道,如："OpenAI_API"
    pub model_channel: String,
    /// 模型 API 密钥
    pub model_api_key: String,
    /// 模型 API 基础 URL
    pub model_api_base_url: String,
}

pub struct SingleAgent<M>{
    agent_id: String,
    memory: Arc<dyn Memory<M>+Send+Sync+'static>,
    session_manager: Arc<dyn SessionMetaManager<SingleAgentSessionConfig>+Send+Sync+'static>,
}

#[async_trait::async_trait]
impl<M> super::Agent for SingleAgent<M> {
    async fn on_env(&self, env: Env, event: EnvEvent) -> anyhow::Result<()> {
        todo!()
    }

    async fn on_session(&self, env: Env, meta: SessionInfo) -> anyhow::Result<Box<dyn Session + Send + 'static>> {
        todo!()
    }

    async fn on_command(&self, env: Env, cmd: Command) -> anyhow::Result<()> {
        todo!()
    }

    async fn exit(&self) -> anyhow::Result<()> {
        todo!()
    }
}