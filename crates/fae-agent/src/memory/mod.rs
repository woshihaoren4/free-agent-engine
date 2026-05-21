mod file_agent_config;
mod file_chat_memory;
mod file_session_config;
mod general_message;

pub use file_agent_config::*;
pub use file_chat_memory::*;
pub use file_session_config::*;
pub use general_message::*;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const EXECUTOR_OPENAI_API_CHANNEL: &str = "OpenAI_API";
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a assistant.";

pub trait MemoryRuler {
    // 内容
    fn as_content(&self) -> String;
    fn from_content(content: String) -> Self;
}

impl MemoryRuler for String {
    fn as_content(&self) -> String {
        self.clone()
    }
    fn from_content(content: String) -> Self {
        content
    }
}

#[derive(Default, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemoryItem<T> {
    /// 记忆 ID
    pub id: String,
    /// 用于关联记忆与具体的会话
    pub session_id: String,
    /// 时间戳
    pub timestamp: u64,
    /// 角色/类型
    pub role: MemoryRole,
    /// 记忆内容
    pub content: T,
}

#[derive(Default, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryRole {
    /// 系统
    System,
    /// 用户
    #[default]
    User,
    /// 助手/模型
    Assistant,
    /// 工具
    Tool,
    /// 自定义
    Custom(String),
}

#[async_trait::async_trait]
pub trait Memory<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static>: Sync {
    /// 加载/获取记忆
    async fn load(
        &self,
        session_id: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryItem<T>>>;

    /// 追加单条记忆
    async fn push(&self, item: MemoryItem<T>) -> anyhow::Result<()>;

    /// 更新单条记忆内容
    async fn update(&self, item: MemoryItem<T>) -> anyhow::Result<()>;

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
