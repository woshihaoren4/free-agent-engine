use std::fmt::Debug;
use crate::{SessionCtl, SessionMD, SessionMetadata};
use serde::{Serialize, de::DeserializeOwned};
use std::sync::Arc;

fn default_session_metadata<T>(user_id: &str, session_id: &str, name: &str) -> anyhow::Result<T>
where
    T: Default + Serialize + DeserializeOwned,
{
    let mut value = serde_json::to_value(T::default())?;
    let Some(map) = value.as_object_mut() else {
        anyhow::bail!("default session metadata must serialize to a JSON object");
    };

    map.insert("id".to_string(), session_id.into());
    map.insert("user_id".to_string(), user_id.into());
    map.insert("name".to_string(), name.into());

    Ok(serde_json::from_value(value)?)
}

//session信息也可以自己管理
#[async_trait::async_trait]
pub trait SessionCtlExt<T>: Debug+ Sync {
    // 加载session列表
    async fn list_ext(&self, user_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<T>>;
    // 加载session详情
    async fn load_ext(&self, user_id: &str, session_id: &str) -> anyhow::Result<Option<T>>;
    // 加载一个默认的session
    async fn must_load_ext(&self, user_id: &str, session_id: &str, name: &str) -> T
    where
        T: Default + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    {
        match self.load_ext(user_id, session_id).await {
            Ok(Some(meta)) => return meta,
            Ok(None) => {}
            Err(e) => panic!(
                "[SessionCtlExt<T>::must_load_ext] failed to load session {}:{}: {:?}",
                user_id, session_id, e
            ),
        }

        let meta: T = default_session_metadata(user_id, session_id, name).unwrap_or_else(|e| {
            panic!(
                "[SessionCtlExt<T>::must_load_ext] failed to build default session {}:{}: {:?}",
                user_id, session_id, e
            )
        });

        if let Err(e) = self.create_ext(meta.clone()).await {
            match self.load_ext(user_id, session_id).await {
                Ok(Some(meta)) => return meta,
                Ok(None) => panic!(
                    "[SessionCtlExt<T>::must_load_ext] failed to create session {}:{}: {:?}",
                    user_id, session_id, e
                ),
                Err(load_err) => panic!(
                    "[SessionCtlExt<T>::must_load_ext] failed to create session {}:{}: {:?}; reload failed: {:?}",
                    user_id, session_id, e, load_err
                ),
            }
        }

        meta
    }
    // 更改session
    async fn update_ext(&self, meta: T) -> anyhow::Result<()>;
    // 创建session
    async fn create_ext(&self, meta: T) -> anyhow::Result<()>;
    // 删除session
    async fn delete_ext(&self, user_id: &str, session_id: &str) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
impl<T> SessionCtl for Arc<dyn SessionCtlExt<T> + Send + 'static>
where
    T: SessionMetadata + Send + Sync + 'static,
{
    async fn list(
        &self,
        user_id: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<SessionMD>> {
        let vec = self.list_ext(user_id, offset, limit).await?;
        Ok(vec
            .into_iter()
            .map(|item| SessionMD::new(item))
            .collect::<Vec<_>>())
    }

    async fn load(&self, user_id: &str, session_id: &str) -> anyhow::Result<Option<SessionMD>> {
        let meta = self.load_ext(user_id, session_id).await?;
        Ok(meta.map(|item| SessionMD::new(item)))
    }

    async fn update(&self, meta: SessionMD) -> anyhow::Result<()> {
        match meta.into_inner::<T>() {
            Ok(md) => self.update_ext(md).await,
            Err(e) => Err(anyhow::anyhow!(
                "[SessionCtlExt<T>::update]session metadata is not of type {:?}",
                e
            )),
        }
    }

    async fn create(&self, meta: SessionMD) -> anyhow::Result<()> {
        match meta.into_inner::<T>() {
            Ok(md) => self.create_ext(md).await,
            Err(e) => Err(anyhow::anyhow!(
                "[SessionCtlExt<T>::create]session metadata is not of type {:?}",
                e
            )),
        }
    }

    async fn delete(&self, user_id: &str, session_id: &str) -> anyhow::Result<()> {
        self.delete_ext(user_id, session_id).await
    }
}
