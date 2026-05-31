use std::sync::Arc;
use crate::{SessionCtl, SessionMetadata, SessionMD};

//session信息也可以自己管理
#[async_trait::async_trait]
pub trait SessionCtlExt<T>: Sync {
    // 加载session列表
    async fn list_ext(&self, user_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<T>>;
    // 加载session详情
    async fn load_ext(&self, user_id: &str, session_id: &str) -> anyhow::Result<Option<T>>;
    // 更改session
    async fn update_ext(&self, user_id: &str, session_id: &str, meta: T) -> anyhow::Result<()>;
    // 创建session
    async fn create_ext(&self, user_id: &str, meta: T) -> anyhow::Result<()>;
    // 删除session
    async fn delete_ext(&self, user_id: &str, session_id: &str) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
impl<T> SessionCtl for Arc<dyn SessionCtlExt<T> + Send + 'static>
where
    T: SessionMetadata + Send + Sync + 'static,
{
    async fn list(&self, user_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<SessionMD>> {
        let vec = self.list_ext(user_id, offset, limit).await?;
        Ok(vec.into_iter().map(|item| SessionMD::new(item)).collect::<Vec<_>>())
    }

    async fn load(&self, user_id: &str, session_id: &str) -> anyhow::Result<Option<SessionMD>> {
        let meta = self.load_ext(user_id, session_id).await?;
        Ok(meta.map(|item| SessionMD::new(item)))
    }

    async fn update(&self, user_id: &str, session_id: &str, meta: SessionMD) -> anyhow::Result<()> {
        match meta.into_inner::<T>(){
            Ok(md) => self.update_ext(user_id, session_id, md).await,
            Err(e) => Err(anyhow::anyhow!("[SessionCtlExt<T>::update]session metadata is not of type {:?}", e)),
        }
    }

    async fn create(&self, user_id: &str, meta: SessionMD) -> anyhow::Result<()> {
        match meta.into_inner::<T>(){
            Ok(md) => self.create_ext(user_id, md).await,
            Err(e) => Err(anyhow::anyhow!("[SessionCtlExt<T>::create]session metadata is not of type {:?}", e)),
        }
    }

    async fn delete(&self, user_id: &str, session_id: &str) -> anyhow::Result<()> {
        self.delete_ext(user_id, session_id).await
    }
}
