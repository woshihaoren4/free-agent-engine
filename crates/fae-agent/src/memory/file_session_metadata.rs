use std::path::PathBuf;
use tokio::fs;
use anyhow::Context;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::memory::{SessionMetaManager};

/// 基于文件系统的会话元数据管理实现
pub struct FileSessionMetaManager<T> {
    dir_path: PathBuf,
    id_extractor: fn(&T) -> String,
    updated_at_extractor: fn(&T) -> u64,
}

impl<T> FileSessionMetaManager<T> {
    /// 创建一个新的 FileSessionMetaManager
    /// 
    /// # 参数
    /// * `dir_path` - 用于存储会话元数据的目录
    /// * `id_extractor` - 用于从元数据中提取 session_id 的函数指针
    /// * `updated_at_extractor` - 用于从元数据中提取 updated_at 的函数指针
    pub async fn new<P: Into<PathBuf>>(
        dir_path: P, 
        id_extractor: fn(&T) -> String,
        updated_at_extractor: fn(&T) -> u64,
    ) -> anyhow::Result<Self> {
        let dir_path = dir_path.into();
        if !dir_path.exists() {
            fs::create_dir_all(&dir_path)
                .await
                .context("Failed to create session metadata directory")?;
        }
        Ok(Self {
            dir_path,
            id_extractor,
            updated_at_extractor,
        })
    }

    fn get_file_path(&self, session_id: &str) -> PathBuf {
        self.dir_path.join(format!("{}.desc", session_id))
    }
}

#[async_trait::async_trait]
impl<T> SessionMetaManager<T> for FileSessionMetaManager<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    async fn session_list(&self, offset: usize, limit: usize) -> anyhow::Result<Vec<T>> {
        let mut entries = fs::read_dir(&self.dir_path)
            .await
            .context("Failed to read directory")?;

        let mut metas = Vec::new();

        while let Some(entry) = entries.next_entry().await.context("Failed to get directory entry")? {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("desc") {
                let content = match fs::read_to_string(&path).await {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if let Ok(meta) = serde_json::from_str::<T>(&content) {
                    metas.push(meta);
                }
            }
        }

        // 按 updated_at 倒序排序
        metas.sort_by(|a, b| (self.updated_at_extractor)(b).cmp(&(self.updated_at_extractor)(a)));

        let start = offset.min(metas.len());
        let end = (offset + limit).min(metas.len());

        Ok(metas[start..end].to_vec())
    }

    async fn update(&self, session_id: &str, meta: T) -> anyhow::Result<()> {
        let file_path = self.get_file_path(session_id);
        if !file_path.exists() {
            anyhow::bail!("Session {} does not exist", session_id);
        }
        let content = serde_json::to_string_pretty(&meta)
            .context("Failed to serialize session meta")?;
        fs::write(file_path, content)
            .await
            .context("Failed to write session file")?;
        Ok(())
    }

    async fn create(&self, meta: T) -> anyhow::Result<()> {
        let session_id = (self.id_extractor)(&meta);
        let file_path = self.get_file_path(&session_id);
        if file_path.exists() {
            anyhow::bail!("Session {} already exists", session_id);
        }
        let content = serde_json::to_string_pretty(&meta)
            .context("Failed to serialize session meta")?;
        fs::write(file_path, content)
            .await
            .context("Failed to write session file")?;
        Ok(())
    }

    async fn delete(&self, session_id: &str) -> anyhow::Result<()> {
        let file_path = self.get_file_path(session_id);
        if file_path.exists() {
            fs::remove_file(file_path)
                .await
                .context("Failed to delete session file")?;
        }
        Ok(())
    }
}


#[derive(Default, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
}

/// 默认的文件会话元数据管理实现类型别名
pub type DefaultFileSessionMetaManager = FileSessionMetaManager<SessionMeta>;

impl FileSessionMetaManager<SessionMeta> {
    /// 为默认的 SessionMeta 创建一个文件管理器
    pub async fn new_default<P: Into<PathBuf>>(dir_path: P) -> anyhow::Result<Self> {
        Self::new(
            dir_path, 
            |meta| meta.id.clone(),
            |meta| meta.updated_at,
        ).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{file_chat_memory::FileChatMemory, Memory, MemoryItem, MemoryRole};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn get_temp_dir(name: &str) -> PathBuf {
        let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("{}_{}", name, time))
    }

    #[tokio::test]
    async fn test_file_session_meta_manager() {
        let dir = get_temp_dir("test_session_meta");
        let manager = DefaultFileSessionMetaManager::new_default(&dir).await.unwrap();

        // Test create
        let meta1 = SessionMeta {
            id: "sess_1".to_string(),
            title: "Session 1".to_string(),
            created_at: 100,
            updated_at: 100,
        };
        manager.create(meta1.clone()).await.unwrap();

        let meta2 = SessionMeta {
            id: "sess_2".to_string(),
            title: "Session 2".to_string(),
            created_at: 200,
            updated_at: 200,
        };
        manager.create(meta2.clone()).await.unwrap();

        // Test list
        let list = manager.session_list(0, 10).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "sess_2");

        // Test update
        let mut meta1_updated = meta1.clone();
        meta1_updated.title = "Session 1 Updated".to_string();
        meta1_updated.updated_at = 300;
        manager.update("sess_1", meta1_updated).await.unwrap();

        let list = manager.session_list(0, 10).await.unwrap();
        assert_eq!(list[0].title, "Session 1 Updated");
        assert_eq!(list[0].id, "sess_1"); // sess_1 updated_at is 300, larger than sess_2 (200)

        // Test delete
        manager.delete("sess_1").await.unwrap();
        let list = manager.session_list(0, 10).await.unwrap();
        assert_eq!(list.len(), 0);

        // Cleanup
        tokio::fs::remove_dir_all(dir).await.ok();
    }

    #[tokio::test]
    async fn test_session_and_memory_integration() {
        let dir = get_temp_dir("test_session_and_memory");
        
        // 1. 初始化 Meta Manager 和 Memory Manager
        let meta_manager = DefaultFileSessionMetaManager::new_default(&dir).await.unwrap();
        let chat_memory = FileChatMemory::<String>::new(&dir).await.unwrap();

        let session_id = "integration_sess_1";

        // 2. 创建 Session Meta
        let meta = SessionMeta {
            id: session_id.to_string(),
            title: "Integration Test Session".to_string(),
            created_at: 1000,
            updated_at: 1000,
        };
        meta_manager.create(meta).await.unwrap();

        // 3. 往这个 Session 写入记忆
        let item1 = MemoryItem {
            id: "msg_1".to_string(),
            session_id: session_id.to_string(),
            timestamp: 1001,
            role: MemoryRole::User,
            content: "Hello from integration test".to_string(),
        };
        chat_memory.push(item1).await.unwrap();
        chat_memory.flush().await.unwrap();

        // 4. 验证 Session 和 Memory 是否正常加载
        let sessions = meta_manager.session_list(0, 10).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session_id);
        assert_eq!(sessions[0].title, "Integration Test Session");

        let loaded_memory = chat_memory.load(session_id, 0, 10).await.unwrap();
        assert_eq!(loaded_memory.len(), 1);
        assert_eq!(loaded_memory[0].content, "Hello from integration test");
        
        // 5. 更新 Session 并且增加新记忆
        let mut updated_meta = sessions[0].clone();
        updated_meta.updated_at = 2000;
        meta_manager.update(session_id, updated_meta).await.unwrap();

        let item2 = MemoryItem {
            id: "msg_2".to_string(),
            session_id: session_id.to_string(),
            timestamp: 2001,
            role: MemoryRole::Assistant,
            content: "Hi there!".to_string(),
        };
        chat_memory.push(item2).await.unwrap();
        chat_memory.flush().await.unwrap();

        let loaded_memory = chat_memory.load(session_id, 0, 10).await.unwrap();
        assert_eq!(loaded_memory.len(), 2);
        
        let sessions = meta_manager.session_list(0, 10).await.unwrap();
        assert_eq!(sessions[0].updated_at, 2000);

        // 6. 清理
        tokio::fs::remove_dir_all(dir).await.ok();
    }
}