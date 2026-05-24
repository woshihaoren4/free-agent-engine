mod file_agent_config;
mod file_chat_memory;
mod file_session_config;
mod openai_api_memory_entry;

pub use file_agent_config::*;
pub use file_chat_memory::*;
pub use file_session_config::*;
pub use openai_api_memory_entry::*;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const EXECUTOR_OPENAI_API_CHANNEL: &str = "OpenAI_API";
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a assistant.";

pub trait MemoryRecord {
    fn id(&self) -> &str;
}

#[async_trait::async_trait]
pub trait Memory<T: MemoryRecord + Serialize + DeserializeOwned + Clone + Send + Sync + 'static>: Sync {
    /// 加载/获取记忆
    async fn load(
        &self,
        session_id: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<T>>;

    /// 追加单条记忆
    async fn push(&self, session_id: &str, item: T) -> anyhow::Result<()>;

    /// 更新单条记忆内容
    async fn update(&self, item:T) -> anyhow::Result<()>;

    /// 删除单条记忆
    async fn delete(&self, session_id: &str, id: &str) -> anyhow::Result<()>;

    /// 重置记忆
    async fn reset(&self, session_id: &str) -> anyhow::Result<()>;

    /// 刷新记忆，将缓存的内容刷新到磁盘中
    async fn flush(&self) -> anyhow::Result<()>;
}

//session信息也可以自己管理
#[async_trait::async_trait]
pub trait SessionConfig<T>: Sync {
    // 加载session列表
    async fn session_list(&self, offset: usize, limit: usize) -> anyhow::Result<Vec<T>>;
    // 加载session详情
    async fn load(&self, session_id: &str) -> anyhow::Result<Option<T>>;
    // 更改session
    async fn update(&self, session_id: &str, meta: T) -> anyhow::Result<()>;
    // 创建session
    async fn create(&self, meta: T) -> anyhow::Result<()>;
    // 删除session
    async fn delete(&self, session_id: &str) -> anyhow::Result<()>;
}

#[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelCallConfig {
    pub model: String,
    //    1:Minimal,2:Low, 3:Medium, 4:High,
    pub reasoning_effort: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>, // min: -2.0, max: 2.0, default: 0

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>, // min: -2.0, max: 2.0, default 0

    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>, // min: 0, max: 2, default: 1,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>, // min: 0, max: 1, default: 1
}

#[async_trait::async_trait]
pub trait AgentConfig: Sync {
    /// 获取智能体名称，唯一标识
    async fn name(&self) -> anyhow::Result<String>;

    /// 获取模型信息
    async fn model(&self) -> anyhow::Result<ModelCallConfig>;

    /// 获取使用的渠道, 默认是 OpenAI_API
    async fn channel(&self) -> anyhow::Result<String> {
        Ok(EXECUTOR_OPENAI_API_CHANNEL.to_string())
    }

    /// 获取系统 prompt
    async fn prompt(&self) -> anyhow::Result<String> {
        Ok(DEFAULT_SYSTEM_PROMPT.to_string())
    }

    /// 获取启用的工具列表
    async fn tools(&self) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// 获取启用的技能 (skill) 列表
    async fn skills(&self) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// 获取配置的 mcp 服务列表
    async fn mcp_servers(&self) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// 获取子 agent 列表
    async fn sub_agents(&self) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// 获取其他自定义配置项
    async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// 设置其他自定义配置项
    async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        Ok(())
    }
}
