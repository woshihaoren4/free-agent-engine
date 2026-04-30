use serde::{Deserialize, Serialize};

#[derive(Default,Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
pub struct MemoryItem<T>{
    // 时间戳
    pub timestamp: u64,
    // 记忆类型
    pub memory_type: MemoryType,
    // 记忆内容
    pub content: T,
}

#[derive(Default,Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
pub enum MemoryType{
    /// 系统记忆
    System,
    /// 用户记忆
    #[default]
    User,
    /// 模型记忆
    Assistant,
    /// 使用工具记忆
    Tool,
    /// 自定义
    Custom,
}

#[async_trait::async_trait]
pub trait Memory<T>{
    /// 加载记忆
    async fn load_memory(&self) -> anyhow::Result<Vec<MemoryItem<T>>>;
    /// 追加记忆
    async fn append_memory(&self, memory: MemoryItem<T>) -> anyhow::Result<()>;
}
