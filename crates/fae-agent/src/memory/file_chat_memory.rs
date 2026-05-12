use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::sync::RwLock;

use super::{Memory, MemoryItem};

/// 基于文件系统的记忆存储实现
pub struct FileChatMemory<T> {
    base_dir: PathBuf,
    // memory map: session_id -> (flushed_count, memory items)
    store: Arc<RwLock<HashMap<String, (usize, Vec<MemoryItem<T>>)>>>,
}

impl<T> FileChatMemory<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// 创建一个新的文件记忆存储实例，管理一个目录
    pub async fn new(base_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let base_dir = base_dir.into();
        let mut store = HashMap::<String, (usize, Vec<MemoryItem<T>>)>::new();

        if !base_dir.exists() {
            tokio::fs::create_dir_all(&base_dir).await?;
        } else {
            let mut entries = tokio::fs::read_dir(&base_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                    let session_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                    if session_id.is_empty() { continue; }
                    
                    let content = tokio::fs::read_to_string(&path).await?;
                    let mut items = Vec::new();
                    for line in content.lines() {
                        let line = line.trim();
                        if line.is_empty() { continue; }
                        if let Ok(item) = serde_json::from_str::<MemoryItem<T>>(line) {
                            items.push(item);
                        }
                    }
                    
                    // Limit to 100 items per session on load
                    if items.len() > 100 {
                        let excess = items.len() - 100;
                        items.drain(0..excess);
                    }
                    let flushed_count = items.len();
                    store.insert(session_id, (flushed_count, items));
                }
            }
        }

        Ok(Self {
            base_dir,
            store: Arc::new(RwLock::new(store)),
        })
    }

    /// 内部方法：刷新指定 session 到文件
    async fn flush_session(&self, session_id: &str) -> anyhow::Result<()> {
        let mut store = self.store.write().await;
        if let Some((flushed_count, items)) = store.get_mut(session_id) {
            if *flushed_count < items.len() {
                let mut content = String::new();
                for item in &items[*flushed_count..] {
                    content.push_str(&serde_json::to_string(item)?);
                    content.push('\n');
                }
                *flushed_count = items.len();
                
                let file_path = self.base_dir.join(format!("{}.jsonl", session_id));
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&file_path)
                    .await
                    .context("Failed to open session memory file for append")?;
                    
                use tokio::io::AsyncWriteExt;
                file.write_all(content.as_bytes()).await?;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl<T> Memory<T> for FileChatMemory<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    async fn session_list(&self, offset: usize, limit: usize) -> anyhow::Result<Vec<String>> {
        let store = self.store.read().await;
        let mut sessions: Vec<_> = store
            .iter()
            .map(|(session_id, (_, items))| {
                let latest_time = items.iter().map(|item| item.timestamp).max().unwrap_or_default();
                (session_id.clone(), latest_time)
            })
            .collect();

        sessions.sort_by(|a, b| b.1.cmp(&a.1));

        let start = offset.min(sessions.len());
        let end = (offset + limit).min(sessions.len());

        Ok(sessions[start..end].iter().map(|(id, _)| id.clone()).collect())
    }

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
        let session_id = item.session_id.clone();
        let should_flush = {
            let mut store = self.store.write().await;
            let (flushed_count, items) = store.entry(session_id.clone()).or_insert_with(|| (0, Vec::new()));
            items.push(item);

            // 限制每个 session 最多存 100 条内容
            if items.len() > 100 {
                let excess = items.len() - 100;
                items.drain(0..excess);
                *flushed_count = flushed_count.saturating_sub(excess);
            }

            items.len() - *flushed_count >= 50
        };

        if should_flush {
            self.flush_session(&session_id).await?;
        }
        Ok(())
    }

    async fn update(
        &self,
        item: MemoryItem<T>,
    ) -> anyhow::Result<()> {
        let session_id = item.session_id.clone();
        let id = item.id.clone();
        let should_flush = {
            let mut store = self.store.write().await;
            if let Some((flushed_count, items)) = store.get_mut(&session_id) {
                if let Some(pos) = items.iter().position(|x| x.id == id) {
                    if pos < *flushed_count {
                        return Err(anyhow::anyhow!("Cannot update an item that has already been flushed to file"));
                    }
                    items[pos] = item;
                    items.len() - *flushed_count >= 50
                } else { false }
            } else { false }
        };

        if should_flush {
            self.flush_session(&session_id).await?;
        }
        Ok(())
    }

    async fn delete(&self, session_id: &str, id: &str) -> anyhow::Result<()> {
        let should_flush = {
            let mut store = self.store.write().await;
            if let Some((flushed_count, items)) = store.get_mut(session_id) {
                if let Some(pos) = items.iter().position(|x| x.id == id) {
                    if pos < *flushed_count {
                        return Err(anyhow::anyhow!("Cannot delete an item that has already been flushed to file"));
                    }
                    items.remove(pos);
                    items.len() - *flushed_count >= 50
                } else { false }
            } else { false }
        };

        if should_flush {
            self.flush_session(session_id).await?;
        }
        Ok(())
    }

    async fn reset(&self, session_id: &str) -> anyhow::Result<()> {
        {
            let mut store = self.store.write().await;
            store.remove(session_id);
        }

        let file_path = self.base_dir.join(format!("{}.jsonl", session_id));
        if file_path.exists() {
            tokio::fs::remove_file(file_path).await?;
        }

        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        let session_ids: Vec<String> = {
            let store = self.store.read().await;
            store.keys().cloned().collect()
        };
        for session_id in session_ids {
            self.flush_session(&session_id).await?;
        }
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

    fn get_temp_dir(name: &str) -> PathBuf {
        let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("{}_{}", name, time));
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        let dir_path = get_temp_dir("test_basic");
        let mem = FileChatMemory::<String>::new(&dir_path).await.unwrap();

        mem.push(mock_item("sess_1", "1", "hello")).await.unwrap();

        let loaded = mem.load("sess_1", 0, 10).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "hello");

        // Should not be in file yet (only 1 item, < 50)
        let file_path = dir_path.join("sess_1.jsonl");
        let content = tokio::fs::read_to_string(&file_path).await.unwrap_or_default();
        assert!(content.is_empty());

        // Flush and verify
        mem.flush().await.unwrap();
        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert!(!content.is_empty());

        tokio::fs::remove_dir_all(dir_path).await.ok();
    }

    #[tokio::test]
    async fn test_update_delete_constraints() {
        let dir_path = get_temp_dir("test_update_delete");
        let mem = FileChatMemory::<String>::new(&dir_path).await.unwrap();

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

        tokio::fs::remove_dir_all(dir_path).await.ok();
    }

    #[tokio::test]
    async fn test_auto_flush_and_limit() {
        let dir_path = get_temp_dir("test_limit");
        let mem = FileChatMemory::<String>::new(&dir_path).await.unwrap();

        for i in 0..105 {
            mem.push(mock_item("sess_1", &i.to_string(), &format!("msg {}", i))).await.unwrap();
        }

        // Session limit keeps only the last 100 items (ids 5 to 104)
        let loaded = mem.load("sess_1", 0, 200).await.unwrap();
        assert_eq!(loaded.len(), 100);
        assert_eq!(loaded[0].id, "5");
        assert_eq!(loaded[99].id, "104");

        // Auto flush (at 50 and 100) should have populated the file
        let file_path = dir_path.join("sess_1.jsonl");
        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert!(!content.is_empty());

        tokio::fs::remove_dir_all(dir_path).await.ok();
    }

    #[tokio::test]
    async fn test_reset_session_and_reload() {
        let dir_path = get_temp_dir("test_reset");
        let mem = FileChatMemory::<String>::new(&dir_path).await.unwrap();

        mem.push(mock_item("sess_1", "1", "msg 1")).await.unwrap();
        mem.push(mock_item("sess_2", "1", "msg 2")).await.unwrap();
        mem.flush().await.unwrap();

        // Reset session 1
        mem.reset("sess_1").await.unwrap();

        // Memory should be cleared for session 1
        let loaded_s1 = mem.load("sess_1", 0, 10).await.unwrap();
        assert!(loaded_s1.is_empty());

        // Create new memory instance to test load from file
        let mem2 = FileChatMemory::<String>::new(&dir_path).await.unwrap();
        let loaded_s1_2 = mem2.load("sess_1", 0, 10).await.unwrap();
        let loaded_s2_2 = mem2.load("sess_2", 0, 10).await.unwrap();

        assert!(loaded_s1_2.is_empty());
        assert_eq!(loaded_s2_2.len(), 1);

        tokio::fs::remove_dir_all(dir_path).await.ok();
    }

    #[tokio::test]
    async fn test_session_list() {
        let dir_path = get_temp_dir("test_session_list");
        let mem = FileChatMemory::<String>::new(&dir_path).await.unwrap();

        let mut item1 = mock_item("sess_1", "1", "msg 1");
        item1.timestamp = 100;
        mem.push(item1).await.unwrap();

        let mut item2 = mock_item("sess_2", "1", "msg 2");
        item2.timestamp = 200;
        mem.push(item2).await.unwrap();

        let list = mem.session_list(0, 10).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], "sess_2");
        assert_eq!(list[1], "sess_1");

        tokio::fs::remove_dir_all(dir_path).await.ok();
    }
}