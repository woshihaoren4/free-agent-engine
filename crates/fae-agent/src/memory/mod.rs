mod file_agent_config;
mod file_chat_memory;
mod file_session_config;
mod openai_api_memory_entry;

use std::any::Any;
pub use file_agent_config::*;
pub use file_chat_memory::*;
pub use file_session_config::*;
pub use openai_api_memory_entry::*;

use crate::Message;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL: &str = "OpenAI-Compatible API";
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a assistant.";

#[async_trait::async_trait]
pub trait Memory<T: Message + Serialize + DeserializeOwned + Clone + Send + Sync + 'static>:
    Sync
{
    ///用户记忆，对应到user prompt
    async fn get_user_info(&self, user_id: &str) -> anyhow::Result<String>;

    ///设置用户记忆,append:是否追加
    async fn set_user_info(&self, user_id: &str, info: String,append:bool) -> anyhow::Result<()>;

    /// 记忆信息，对应到system prompt
    async fn metadata(&self, user_id: &str, session_id: &str) -> anyhow::Result<String>;

    /// 加载/获取记忆
    async fn load(&self, user_id: &str, session_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<T>>;

    /// 追加单条记忆
    async fn push(&self, user_id: &str, session_id: &str, item: T) -> anyhow::Result<()>;

    /// 更新单条记忆内容
    async fn update(&self, user_id: &str, item: T) -> anyhow::Result<()>;

    /// 删除单条记忆
    async fn delete(&self, user_id: &str, session_id: &str, id: &str) -> anyhow::Result<()>;

    /// 重置记忆
    async fn reset(&self, user_id: &str, session_id: &str) -> anyhow::Result<()>;

    /// 刷新记忆，将缓存的内容刷新到磁盘中
    async fn flush(&self) -> anyhow::Result<()>;
}

//session信息也可以自己管理
#[async_trait::async_trait]
pub trait SessionConfig<T>: Sync {
    // 加载session列表
    async fn session_list(&self, user_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<T>>;
    // 加载session详情
    async fn load(&self, user_id: &str, session_id: &str) -> anyhow::Result<Option<T>>;
    // 更改session
    async fn update(&self, user_id: &str, session_id: &str, meta: T) -> anyhow::Result<()>;
    // 创建session
    async fn create(&self, user_id: &str, meta: T) -> anyhow::Result<()>;
    // 删除session
    async fn delete(&self, user_id: &str, session_id: &str) -> anyhow::Result<()>;
}

#[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelCallConfig {
    pub model: String,
    // 模型执行器渠道
    pub channel: String,
    // 最大聊天历史记录轮数
    pub max_chat_history_round: u32,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolConfig {
    pub name: String,
    pub channel: String,
}
impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            name: "".into(),
            channel: "default".to_string(),
        }
    }
}
impl ToolConfig {
    pub fn new<S: Into<String>>(name: S) -> Self {
        Self {
            name: name.into(),
            channel: "default".to_string(),
        }
    }
    pub fn with_channel(self, channel: String) -> Self {
        Self {
            name: self.name,
            channel,
        }
    }
}

#[async_trait::async_trait]
pub trait AgentConfig:Sync {
    /// 获取智能体名称，唯一标识
    fn name(&self) -> String;

    /// 获取模型信息
    fn model(&self) -> ModelCallConfig;

    /// 获取系统 prompt
    fn prompt(&self) -> String {
        DEFAULT_SYSTEM_PROMPT.to_string()
    }

    /// 获取启用的工具列表
    fn tools(&self) -> Vec<ToolConfig> {
        Vec::new()
    }

    /// 获取启用的技能 (skill) 列表
    fn skills(&self) -> Vec<String> {
        Vec::new()
    }

    /// 获取配置的 mcp 服务列表
    fn mcp_servers(&self) -> Vec<String> {
        Vec::new()
    }

    /// 获取子 agent 列表
    fn sub_agents(&self) -> Vec<String> {
        Vec::new()
    }

    /// 获取其他自定义配置项
    fn get(&self, key: &str) ->Option<String> {
        None
    }

    /// agent信息, 包括workspace相关
    fn metadata(&self,id:&str) -> String {
        "".to_string()
    }

    async fn init(&mut self, id:&str, workspace:&str, cfg:serde_json::Value) -> anyhow::Result<()>;
}

