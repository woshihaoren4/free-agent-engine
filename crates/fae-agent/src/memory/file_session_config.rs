use anyhow::Context;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::PathBuf;
use tokio::fs;

use crate::memory::SessionConfig;

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
        let dir_path = dir_path.into().join("memory");
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

    fn get_file_path(&self, user_id: &str, session_id: &str) -> PathBuf {
        self.dir_path.join(user_id).join(format!("{}.desc", session_id))
    }
}

#[async_trait::async_trait]
impl<T> SessionConfig<T> for FileSessionMetaManager<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    async fn session_list(&self, user_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<T>> {
        let user_dir = self.dir_path.join(user_id);
        if !user_dir.exists() {
            return Ok(vec![]);
        }
        let mut entries = fs::read_dir(&user_dir)
            .await
            .context("Failed to read directory")?;

        let mut metas = Vec::new();

        while let Some(entry) = entries
            .next_entry()
            .await
            .context("Failed to get directory entry")?
        {
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

    async fn load(&self, user_id: &str, session_id: &str) -> anyhow::Result<Option<T>> {
        let file_path = self.get_file_path(user_id, session_id);
        if !file_path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&file_path)
            .await
            .context("Failed to read session file")?;
        let meta =
            serde_json::from_str::<T>(&content).context("Failed to deserialize session meta")?;
        Ok(Some(meta))
    }

    async fn update(&self, user_id: &str, session_id: &str, meta: T) -> anyhow::Result<()> {
        let file_path = self.get_file_path(user_id, session_id);
        if !file_path.exists() {
            anyhow::bail!("Session {} does not exist", session_id);
        }
        let content =
            serde_json::to_string_pretty(&meta).context("Failed to serialize session meta")?;
        fs::write(file_path, content)
            .await
            .context("Failed to write session file")?;
        Ok(())
    }

    async fn create(&self, user_id: &str, meta: T) -> anyhow::Result<()> {
        let session_id = (self.id_extractor)(&meta);
        let user_dir = self.dir_path.join(user_id);
        if !user_dir.exists() {
            fs::create_dir_all(&user_dir).await?;
        }
        let file_path = self.get_file_path(user_id, &session_id);
        if file_path.exists() {
            anyhow::bail!("Session {} already exists", session_id);
        }
        let content =
            serde_json::to_string_pretty(&meta).context("Failed to serialize session meta")?;
        fs::write(file_path, content)
            .await
            .context("Failed to write session file")?;
        Ok(())
    }

    async fn delete(&self, user_id: &str, session_id: &str) -> anyhow::Result<()> {
        let file_path = self.get_file_path(user_id, session_id);
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
        Self::new(dir_path, |meta| meta.id.clone(), |meta| meta.updated_at).await
    }
}
