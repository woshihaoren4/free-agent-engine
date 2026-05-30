mod file_agent_config;
mod file_chat_memory;
mod openai_api_memory_entry;
mod memory_message_ext;

pub use file_agent_config::*;
pub use file_chat_memory::*;
pub use crate::session::file_session_ctl::*;
pub use openai_api_memory_entry::*;
pub use memory_message_ext::*;

use crate::Msg;

pub const EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL: &str = "OpenAI-Compatible API";
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a claw.";

#[async_trait::async_trait]
pub trait Memory: Sync {
    ///用户记忆，对应到user prompt
    async fn get_user_info(&self, user_id: &str) -> anyhow::Result<String>;

    ///设置用户记忆,append:是否追加
    async fn set_user_info(&self, user_id: &str, info: String,append:bool) -> anyhow::Result<()>;

    /// 记忆信息，对应到system prompt
    async fn metadata(&self, user_id: &str, session_id: &str) -> anyhow::Result<String>;

    /// 加载/获取记忆
    async fn load(&self, user_id: &str, session_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<Msg>>;

    /// 追加单条记忆
    async fn push(&self, user_id: &str, session_id: &str, item: Msg) -> anyhow::Result<()>;

    /// 更新单条记忆内容
    async fn update(&self, user_id: &str, item: Msg) -> anyhow::Result<()>;

    /// 删除单条记忆
    async fn delete(&self, user_id: &str, session_id: &str, id: &str) -> anyhow::Result<()>;

    /// 重置记忆
    async fn reset(&self, user_id: &str, session_id: &str) -> anyhow::Result<()>;

    /// 刷新记忆，将缓存的内容刷新到磁盘中
    async fn flush(&self) -> anyhow::Result<()>;
}






