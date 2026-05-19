mod file_chat_memory;
mod file_session_metadata;
mod general_message;

pub use file_chat_memory::*;
pub use file_session_metadata::*;
pub use general_message::*;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

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
pub trait SessionMetaManager<T>: Sync {
    // 加载session列表
    async fn session_list(&self, offset: usize, limit: usize) -> anyhow::Result<Vec<T>>;
    // 更改session
    async fn update(&self, session_id: &str, meta: T) -> anyhow::Result<()>;
    // 创建session
    async fn create(&self, meta: T) -> anyhow::Result<()>;
    // 删除session
    async fn delete(&self, session_id: &str) -> anyhow::Result<()>;
}
