use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::Memory;
use crate::Message;
use anyhow::Context;
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::RwLock;

/// 基于文件系统的记忆存储实现
pub struct FileChatMemory<T> {
    base_dir: PathBuf,
    // memory map: session_id -> (flushed_count, memory items)
    store: Arc<RwLock<HashMap<String, (usize, Vec<T>)>>>,
}

impl<T> FileChatMemory<T>
where
    T: Message + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// 创建一个新的文件记忆存储实例，管理一个目录
    pub async fn new(base_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let base_dir = base_dir.into();
        let mut store = HashMap::<String, (usize, Vec<T>)>::new();

        if !base_dir.exists() {
            tokio::fs::create_dir_all(&base_dir).await?;
        } else {
            let mut entries = tokio::fs::read_dir(&base_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
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
                    .context(format!(
                        "Failed to open session memory file for append: {:?}",
                        file_path
                    ))?;

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
    T: Message + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    async fn load(&self, session_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<T>> {
        let store = self.store.read().await;
        if let Some((_, items)) = store.get(session_id) {
            let start = offset.min(items.len());
            let end = (offset + limit).min(items.len());
            return Ok(items[start..end].to_vec());
        }
        Ok(vec![])
    }

    async fn push(&self, session_id: &str, item: T) -> anyhow::Result<()> {
        let session_id_owned = session_id.to_string();
        let should_flush = {
            let mut store = self.store.write().await;
            let (flushed_count, items) = store
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
            self.flush_session(&session_id_owned).await?;
        }
        Ok(())
    }

    async fn update(&self, item: T) -> anyhow::Result<()> {
        let id = item.id().to_string();
        let mut target_session = None;
        let should_flush = {
            let mut store = self.store.write().await;
            // Find which session contains this item
            let mut found = false;
            let mut flush_needed = false;
            for (session_id, (flushed_count, items)) in store.iter_mut() {
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
            if !found {
                return Ok(()); // Item not found
            }
            flush_needed
        };

        if should_flush {
            if let Some(sid) = target_session {
                self.flush_session(&sid).await?;
            }
        }
        Ok(())
    }

    async fn delete(&self, session_id: &str, id: &str) -> anyhow::Result<()> {
        let should_flush = {
            let mut store = self.store.write().await;
            if let Some((flushed_count, items)) = store.get_mut(session_id) {
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
