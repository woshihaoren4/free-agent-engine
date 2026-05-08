use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Context;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::sync::RwLock;

use super::{Memory, MemoryItem};

/// 基于文件系统的记忆存储实现
pub struct FileChatMemory<T> {
    file_path: PathBuf,
    // memory map: session_id -> (flushed_count, memory items)
    store: Arc<RwLock<HashMap<String, (usize, Vec<MemoryItem<T>>)>>>,
    // count of unsaved changes
    unsaved_count: Arc<AtomicUsize>,
}

impl<T> FileChatMemory<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// 创建一个新的文件记忆存储实例
    pub async fn new(file_path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let file_path = file_path.into();
        let mut store = HashMap::<String, (usize, Vec<MemoryItem<T>>)>::new();
        let mut is_jsonl = true;

        if file_path.exists() {
            let content = tokio::fs::read_to_string(&file_path).await?;
            let content = content.trim();
            if !content.is_empty() {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() { continue; }
                    if line.starts_with(r#"{"__reset_session__":"#) {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                            if let Some(session_id) = val.get("__reset_session__").and_then(|v| v.as_str()) {
                                store.remove(session_id);
                                continue;
                            }
                        }
                    }
                    match serde_json::from_str::<MemoryItem<T>>(line) {
                        Ok(item) => {
                            store.entry(item.session_id.clone()).or_insert_with(|| (0, Vec::new())).1.push(item);
                        }
                        Err(_) => {
                            is_jsonl = false;
                            break;
                        }
                    }
                }
                
                if !is_jsonl {
                    // Fallback to original HashMap format
                    let old_store: HashMap<String, Vec<MemoryItem<T>>> = serde_json::from_str(content).context("Failed to parse memory file")?;
                    store = old_store.into_iter().map(|(k, v)| (k, (0, v))).collect();
                }
                
                // Limit to 100 items per session on load
                for (flushed_count, items) in store.values_mut() {
                    if items.len() > 100 {
                        let excess = items.len() - 100;
                        items.drain(0..excess);
                    }
                    *flushed_count = items.len();
                }
            }
        } else {
            if let Some(parent) = file_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        let mem = Self {
            file_path,
            store: Arc::new(RwLock::new(store)),
            unsaved_count: Arc::new(AtomicUsize::new(0)),
        };
        
        if !is_jsonl {
            let mut content = String::new();
            {
                let store_read = mem.store.read().await;
                for (_, items) in store_read.values() {
                    for item in items {
                        content.push_str(&serde_json::to_string(item)?);
                        content.push('\n');
                    }
                }
            }
            tokio::fs::write(&mem.file_path, content).await?;
        }
        
        Ok(mem)
    }

    /// 内部方法：检查是否需要刷新到文件
    async fn check_flush(&self) -> anyhow::Result<()> {
        let count = self.unsaved_count.fetch_add(1, Ordering::SeqCst) + 1;
        if count >= 50 {
            self.flush().await?;
        }
        Ok(())
    }

}

#[async_trait::async_trait]
impl<T> Memory<T> for FileChatMemory<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    async fn load(
        &self,
        session_id: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryItem<T>>> {
        let store = self.store.read().await;
        if let Some((_, items)) = store.get(session_id) {
            let start = offset.min(items.len());
            let end = (offset + limit).min(items.len());
            return Ok(items[start..end].to_vec());
        }
        Ok(vec![])
    }

    async fn push(&self, item: MemoryItem<T>) -> anyhow::Result<()> {
        {
            let session_id = item.session_id.as_str();
            let mut store = self.store.write().await;
            let (flushed_count, items) = store.entry(session_id.to_string()).or_insert_with(|| (0, Vec::new()));
            items.push(item);
            
            // 限制每个 session 最多存 100 条内容
            if items.len() > 100 {
                let excess = items.len() - 100;
                items.drain(0..excess);
                *flushed_count = flushed_count.saturating_sub(excess);
            }
        }

        self.check_flush().await?;
        Ok(())
    }

    async fn update(
        &self,
        item: MemoryItem<T>,
    ) -> anyhow::Result<()> {
        let session_id = item.session_id.as_str();
        let id = item.id.as_str();
        let updated = {
            let mut store = self.store.write().await;
            if let Some((flushed_count, items)) = store.get_mut(session_id) {
                if let Some(pos) = items.iter().position(|x| x.id == id) {
                    if pos < *flushed_count {
                        return Err(anyhow::anyhow!("Cannot update an item that has already been flushed to file"));
                    }
                    items[pos] = item;
                    true
                } else { false }
            } else { false }
        };
        
        if updated {
            self.check_flush().await?;
        }
        Ok(())
    }

    async fn delete(&self, session_id: &str, id: &str) -> anyhow::Result<()> {
        let deleted = {
            let mut store = self.store.write().await;
            if let Some((flushed_count, items)) = store.get_mut(session_id) {
                if let Some(pos) = items.iter().position(|x| x.id == id) {
                    if pos < *flushed_count {
                        return Err(anyhow::anyhow!("Cannot delete an item that has already been flushed to file"));
                    }
                    items.remove(pos);
                    true
                } else { false }
            } else { false }
        };
        
        if deleted {
            self.check_flush().await?;
        }
        Ok(())
    }

    async fn reset(&self, session_id: &str) -> anyhow::Result<()> {
        {
            let mut store = self.store.write().await;
            store.remove(session_id);
        }
        
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
            .await
            .context("Failed to open memory file for reset marker")?;
            
        use tokio::io::AsyncWriteExt;
        let marker = format!("{{\"__reset_session__\":\"{}\"}}\n", session_id);
        file.write_all(marker.as_bytes()).await?;
        
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        let mut store = self.store.write().await;
        let mut content = String::new();
        
        for (flushed_count, items) in store.values_mut() {
            if *flushed_count < items.len() {
                for item in &items[*flushed_count..] {
                    content.push_str(&serde_json::to_string(item)?);
                    content.push('\n');
                }
                *flushed_count = items.len();
            }
        }
        
        if !content.is_empty() {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.file_path)
                .await
                .context("Failed to open memory file for append")?;
                
            use tokio::io::AsyncWriteExt;
            file.write_all(content.as_bytes()).await?;
        }
        
        self.unsaved_count.store(0, Ordering::SeqCst);
        Ok(())
    }
}

/// 常用的大模型聊天内容
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatContent {
        /// 文本内容
        #[serde(skip_serializing_if = "Option::is_none")]
        pub text: Option<String>,
        /// 工具调用请求（当模型决定调用工具时）
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_calls: Option<Vec<ToolCall>>,
        /// 工具执行结果（当作为 Tool 角色返回结果时）
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_result: Option<String>,
}

/// 工具调用信息
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
        /// 工具调用 ID
        pub id: String,
        /// 工具名称
        pub name: String,
        /// 工具参数 (JSON 字符串)
        pub arguments: String,
}

/// 默认的文件聊天记忆实现类型别名
pub type FileChatMemoryImpl = FileChatMemory<ChatContent>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryRole;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn get_temp_file(name: &str) -> PathBuf {
        let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("{}_{}.jsonl", name, time))
    }

    fn mock_item(session: &str, id: &str, content: &str) -> MemoryItem<String> {
        MemoryItem {
            id: id.to_string(),
            session_id: session.to_string(),
            timestamp: 0,
            role: MemoryRole::User,
            content: content.to_string(),
        }
    }

    #[tokio::test]
    async fn test_basic_push_load_flush() {
        let file_path = get_temp_file("test_basic");
        let mem = FileChatMemory::<String>::new(&file_path).await.unwrap();

        mem.push(mock_item("sess_1", "1", "hello")).await.unwrap();

        let loaded = mem.load("sess_1", 0, 10).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "hello");

        // Should not be in file yet (only 1 item, < 50)
        let content = tokio::fs::read_to_string(&file_path).await.unwrap_or_default();
        assert!(content.is_empty());

        // Flush and verify
        mem.flush().await.unwrap();
        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert!(!content.is_empty());

        tokio::fs::remove_file(file_path).await.ok();
    }

    #[tokio::test]
    async fn test_update_delete_constraints() {
        let file_path = get_temp_file("test_update_delete");
        let mem = FileChatMemory::<String>::new(&file_path).await.unwrap();

        mem.push(mock_item("sess_1", "1", "hello")).await.unwrap();

        // Update unsaved item -> Success
        assert!(mem.update(mock_item("sess_1", "1", "world")).await.is_ok());
        let loaded = mem.load("sess_1", 0, 10).await.unwrap();
        assert_eq!(loaded[0].content, "world");

        // Flush item to disk
        mem.flush().await.unwrap();

        // Update saved item -> Error
        assert!(mem.update(mock_item("sess_1", "1", "fail")).await.is_err());

        // Delete saved item -> Error
        assert!(mem.delete("sess_1", "1").await.is_err());

        tokio::fs::remove_file(file_path).await.ok();
    }

    #[tokio::test]
    async fn test_auto_flush_and_limit() {
        let file_path = get_temp_file("test_limit");
        let mem = FileChatMemory::<String>::new(&file_path).await.unwrap();

        for i in 0..105 {
            mem.push(mock_item("sess_1", &i.to_string(), &format!("msg {}", i))).await.unwrap();
        }

        // Session limit keeps only the last 100 items (ids 5 to 104)
        let loaded = mem.load("sess_1", 0, 200).await.unwrap();
        assert_eq!(loaded.len(), 100);
        assert_eq!(loaded[0].id, "5");
        assert_eq!(loaded[99].id, "104");

        // Auto flush (at 50 and 100) should have populated the file
        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert!(!content.is_empty());

        tokio::fs::remove_file(file_path).await.ok();
    }

    #[tokio::test]
    async fn test_reset_session_and_reload() {
        let file_path = get_temp_file("test_reset");
        let mem = FileChatMemory::<String>::new(&file_path).await.unwrap();

        mem.push(mock_item("sess_1", "1", "msg 1")).await.unwrap();
        mem.push(mock_item("sess_2", "1", "msg 2")).await.unwrap();
        mem.flush().await.unwrap();

        // Reset session 1
        mem.reset("sess_1").await.unwrap();

        // Memory should be cleared for session 1
        let loaded_s1 = mem.load("sess_1", 0, 10).await.unwrap();
        assert!(loaded_s1.is_empty());

        // Create new memory instance to test load from file with reset marker
        let mem2 = FileChatMemory::<String>::new(&file_path).await.unwrap();
        let loaded_s1_2 = mem2.load("sess_1", 0, 10).await.unwrap();
        let loaded_s2_2 = mem2.load("sess_2", 0, 10).await.unwrap();

        assert!(loaded_s1_2.is_empty());
        assert_eq!(loaded_s2_2.len(), 1);

        tokio::fs::remove_file(file_path).await.ok();
    }
}