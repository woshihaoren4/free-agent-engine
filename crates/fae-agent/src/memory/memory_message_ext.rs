use std::marker::PhantomData;
use std::sync::Arc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use crate::{Memory, Message, Msg};

#[async_trait::async_trait]
pub trait MemoryMessageExt<T: Message + Serialize + DeserializeOwned + Clone + Send + Sync + 'static>:
Sync
{
    ///用户记忆，对应到user prompt
    async fn get_user_info_ext(&self, user_id: &str) -> anyhow::Result<String>;

    ///设置用户记忆,append:是否追加
    async fn set_user_info_ext(&self, user_id: &str, info: String,append:bool) -> anyhow::Result<()>;

    /// 记忆信息，对应到system prompt
    async fn metadata_ext(&self, user_id: &str, session_id: &str) -> anyhow::Result<String>;

    /// 加载/获取记忆
    async fn load_ext(&self, user_id: &str, session_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<T>>;

    /// 追加单条记忆
    async fn push_ext(&self, user_id: &str, session_id: &str, item: T) -> anyhow::Result<()>;

    /// 更新单条记忆内容
    async fn update_ext(&self, user_id: &str, item: T) -> anyhow::Result<()>;

    /// 删除单条记忆
    async fn delete_ext(&self, user_id: &str, session_id: &str, id: &str) -> anyhow::Result<()>;

    /// 重置记忆
    async fn reset_ext(&self, user_id: &str, session_id: &str) -> anyhow::Result<()>;

    /// 刷新记忆，将缓存的内容刷新到磁盘中
    async fn flush_ext(&self) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
impl<T> Memory for Arc<dyn MemoryMessageExt<T> + Send + 'static>
where
    T: Message + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    async fn get_user_info(&self, user_id: &str) -> anyhow::Result<String> {
        self.get_user_info_ext(user_id).await
    }

    async fn set_user_info(&self, user_id: &str, info: String, append: bool) -> anyhow::Result<()> {
        self.set_user_info_ext(user_id,info,append).await
    }

    async fn metadata(&self, user_id: &str, session_id: &str) -> anyhow::Result<String> {
        self.metadata_ext(user_id,session_id).await
    }

    async fn load(&self, user_id: &str, session_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<Msg>> {
        let vec = self.load_ext(user_id, session_id, offset, limit).await?;
        Ok(vec.into_iter().map(|item| Msg::new(item)).collect::<Vec<_>>())
    }

    async fn push(&self, user_id: &str, session_id: &str, msg: Msg) -> anyhow::Result<()> {
        match msg.into_inner::<T>(){
            Ok(msg) => self.push_ext(user_id, session_id, msg).await,
            Err(e) => Err(anyhow::anyhow!("[MemoryMessageExtImpl] msg {:?} is not T", e)),
        }
    }

    async fn update(&self, user_id: &str, msg: Msg) -> anyhow::Result<()> {
        match msg.into_inner::<T>(){
            Ok(msg) => self.update_ext(user_id, msg).await,
            Err(e) => Err(anyhow::anyhow!("[MemoryMessageExtImpl] msg {:?} is not T", e)),
        }
    }

    async fn delete(&self, user_id: &str, session_id: &str, id: &str) -> anyhow::Result<()> {
        self.delete_ext(user_id, session_id, id).await
    }

    async fn reset(&self, user_id: &str, session_id: &str) -> anyhow::Result<()> {
        self.reset_ext(user_id, session_id).await
    }

    async fn flush(&self) -> anyhow::Result<()> {
        self.flush_ext().await
    }
}
