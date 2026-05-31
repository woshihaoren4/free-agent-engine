use serde::{Deserialize, Serialize};
use crate::{DEFAULT_SYSTEM_PROMPT};

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

    #[deprecated]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

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

    /// 获取智能体描述
    fn desc(&self) -> String {
        String::new()
    }

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