use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::MemoryMessageExt;
use crate::Message;
use anyhow::Context;
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::RwLock;

/// 基于文件系统的记忆存储实现
#[derive(Debug)]
pub struct FileChatMemory<T> {
    agent_dir: PathBuf,
    memory_dir: PathBuf,
    // memory map: user_id -> session_id -> (flushed_count, memory items)
    store: Arc<RwLock<HashMap<String, HashMap<String, (usize, Vec<T>)>>>>,
}

impl<T> FileChatMemory<T>
where
    T: Message + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// 创建一个新的文件记忆存储实例，管理一个目录
    pub async fn new(base_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let base_dir = base_dir.into();
        let memory_dir = base_dir.join("memory");
        let mut store = HashMap::<String, HashMap<String, (usize, Vec<T>)>>::new();

        if !memory_dir.exists() {
            tokio::fs::create_dir_all(&memory_dir).await?;
        } else {
            let mut entries = tokio::fs::read_dir(&memory_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let user_path = entry.path();
                if user_path.is_dir() {
                    let user_id = user_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    if user_id.is_empty() {
                        continue;
                    }
                    let mut user_store = HashMap::new();
                    let mut user_entries = tokio::fs::read_dir(&user_path).await?;
                    while let Some(user_entry) = user_entries.next_entry().await? {
                        let path = user_entry.path();
                        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                            let session_id = path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("")
                                .to_string();
                            if session_id.is_empty() {
                                continue;
                            }

                            let content = tokio::fs::read_to_string(&path).await?;
                            let mut items = Vec::new();
                            for line in content.lines() {
                                let line = line.trim();
                                if line.is_empty() {
                                    continue;
                                }
                                if let Ok(item) = serde_json::from_str::<T>(line) {
                                    items.push(item);
                                }
                            }

                            if items.len() > 100 {
                                let excess = items.len() - 100;
                                items.drain(0..excess);
                            }
                            let flushed_count = items.len();
                            user_store.insert(session_id, (flushed_count, items));
                        }
                    }
                    store.insert(user_id, user_store);
                }
            }
        }

        Ok(Self {
            memory_dir,
            agent_dir: base_dir.into(),
            store: Arc::new(RwLock::new(store)),
        })
    }

    /// 内部方法：刷新指定 session 到文件
    async fn flush_session(&self, user_id: &str, session_id: &str) -> anyhow::Result<()> {
        let mut store = self.store.write().await;
        if let Some(user_store) = store.get_mut(user_id) {
            if let Some((flushed_count, items)) = user_store.get_mut(session_id) {
                if *flushed_count < items.len() {
                    let mut content = String::new();
                    for item in &items[*flushed_count..] {
                        content.push_str(&serde_json::to_string(item)?);
                        content.push('\n');
                    }
                    *flushed_count = items.len();

                    let user_dir = self.memory_dir.join(user_id);
                    if !user_dir.exists() {
                        tokio::fs::create_dir_all(&user_dir).await?;
                    }
                    let file_path = user_dir.join(format!("{}.jsonl", session_id));
                    let mut file = tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&file_path)
                        .await
                        .context(format!(
                            "Failed to open session memory file for append: {:?}",
                            file_path
                        ))?;

                    use tokio::io::AsyncWriteExt;
                    file.write_all(content.as_bytes()).await?;
                }
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl<T> MemoryMessageExt<T> for FileChatMemory<T>
where
    T: Message + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// 用户记忆
    async fn on_user_info(&self, user_id: &str) -> anyhow::Result<String> {
        let user_dir = self.memory_dir.join(user_id);
        let user_file = user_dir.join("user.txt");
        let mem = if user_file.exists() {
            tokio::fs::read_to_string(&user_file).await?
        } else {
            "".to_string()
        };
        let mut info = "\n---\n## About user memory:".to_string();
        info.push_str("  - You must update your memory once you have a clear understanding of some user attributes or preferences.");
        info.push_str(&format!("\nStorage file path: `{}`.", user_file.display()));
        info.push_str(&format!("\nRecorded content: \n{}", mem));
        Ok(info)
    }

    /// 设置用户记忆,append:是否追加
    async fn set_user_info_ext(
        &self,
        user_id: &str,
        info: String,
        append: bool,
    ) -> anyhow::Result<()> {
        let user_dir = self.memory_dir.join(user_id);
        if !user_dir.exists() {
            tokio::fs::create_dir_all(&user_dir).await?;
        }
        let user_file = user_dir.join("user.txt");
        if append {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&user_file)
                .await?;
            use tokio::io::AsyncWriteExt;
            file.write_all(info.as_bytes()).await?;
            file.write_all(b"\n").await?;
        } else {
            tokio::fs::write(&user_file, info).await?;
        }
        Ok(())
    }

    /// 记忆信息
    async fn on_metadata(&self, user_id: &str, session_id: &str) -> anyhow::Result<String> {
        let mut info = "\n## Your memory Metadata:".to_string();
        info.push_str(&format!(
            "\n - This dialogue identifier $SESSION_ID: `{}`",
            session_id
        ));
        info.push_str(&format!(
            "\n - session description file path: {}/{}/{}.desc",
            self.memory_dir.display(),
            user_id,
            session_id
        ));
        Ok(info)
    }
    async fn on_load(
        &self,
        user_id: &str,
        session_id: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<T>> {
        let store = self.store.read().await;
        if let Some(user_store) = store.get(user_id) {
            if let Some((_, items)) = user_store.get(session_id) {
                let start = offset.min(items.len());
                let end = (offset + limit).min(items.len());
                return Ok(items[start..end].to_vec());
            }
        }
        Ok(vec![])
    }

    async fn on_push(&self, user_id: &str, session_id: &str, item: T) -> anyhow::Result<()> {
        let session_id_owned = session_id.to_string();
        let user_id_owned = user_id.to_string();
        let should_flush = {
            let mut store = self.store.write().await;
            let user_store = store
                .entry(user_id_owned.clone())
                .or_insert_with(HashMap::new);
            let (flushed_count, items) = user_store
                .entry(session_id_owned.clone())
                .or_insert_with(|| (0, Vec::new()));
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
            self.flush_session(&user_id_owned, &session_id_owned)
                .await?;
        }
        Ok(())
    }

    async fn on_update(&self, user_id: &str, item: T) -> anyhow::Result<()> {
        let id = item.id().to_string();
        let mut target_session = None;
        let should_flush = {
            let mut store = self.store.write().await;
            // Find which session contains this item
            let mut found = false;
            let mut flush_needed = false;
            if let Some(user_store) = store.get_mut(user_id) {
                for (session_id, (flushed_count, items)) in user_store.iter_mut() {
                    if let Some(pos) = items.iter().position(|x| x.id() == id) {
                        if pos < *flushed_count {
                            return Err(anyhow::anyhow!(
                                "Cannot update an item that has already been flushed to file"
                            ));
                        }
                        items[pos] = item.clone();
                        flush_needed = items.len() - *flushed_count >= 50;
                        target_session = Some(session_id.clone());
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                return Ok(()); // Item not found
            }
            flush_needed
        };

        if should_flush {
            if let Some(sid) = target_session {
                self.flush_session(user_id, &sid).await?;
            }
        }
        Ok(())
    }

    async fn on_delete(&self, user_id: &str, session_id: &str, id: &str) -> anyhow::Result<()> {
        let should_flush = {
            let mut store = self.store.write().await;
            if let Some(user_store) = store.get_mut(user_id) {
                if let Some((flushed_count, items)) = user_store.get_mut(session_id) {
                    if let Some(pos) = items.iter().position(|x| x.id() == id) {
                        if pos < *flushed_count {
                            return Err(anyhow::anyhow!(
                                "Cannot delete an item that has already been flushed to file"
                            ));
                        }
                        items.remove(pos);
                        items.len() - *flushed_count >= 50
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        };

        if should_flush {
            self.flush_session(user_id, session_id).await?;
        }
        Ok(())
    }

    async fn on_reset(&self, user_id: &str, session_id: &str) -> anyhow::Result<()> {
        {
            let mut store = self.store.write().await;
            if let Some(user_store) = store.get_mut(user_id) {
                user_store.remove(session_id);
            }
        }

        let file_path = self
            .memory_dir
            .join(user_id)
            .join(format!("{}.jsonl", session_id));
        if file_path.exists() {
            tokio::fs::remove_file(file_path).await?;
        }

        Ok(())
    }

    async fn on_flush(&self) -> anyhow::Result<()> {
        let sessions: Vec<(String, String)> = {
            let store = self.store.read().await;
            let mut res = Vec::new();
            for (uid, user_store) in store.iter() {
                for sid in user_store.keys() {
                    res.push((uid.clone(), sid.clone()));
                }
            }
            res
        };
        for (uid, sid) in sessions {
            self.flush_session(&uid, &sid).await?;
        }
        Ok(())
    }
}
