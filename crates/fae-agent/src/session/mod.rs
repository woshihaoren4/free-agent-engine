pub mod file_session_ctl;
mod session_ctl_ext;
mod session_event_layer;
mod session_trait_ext;
mod single_session_metadata;

use crate::Msg;
use crate::error::Error;
pub use session_ctl_ext::*;
pub use session_event_layer::*;
pub use session_trait_ext::*;
pub use single_session_metadata::*;
use std::any::Any;
use std::collections::HashMap;
use std::fmt::Debug;
use tokio_stream::Stream;
use wd_tools::PFErr;

// ----------------------  通信会话 -----------------------------

/// 会话 trait，定义智能体与外部交互的接口
#[async_trait::async_trait]
pub trait Session: Sync {
    /// 同步调用，返回单个消息
    async fn call(&mut self, _input: Msg) -> anyhow::Result<Msg> {
        anyhow::Error::from(Error::Session("Session.call".into())).err()
    }

    /// 调用并返回流
    async fn call_stream(
        &mut self,
        _input: Msg,
    ) -> anyhow::Result<Box<dyn Stream<Item = Msg> + Send + Sync>> {
        anyhow::Error::from(Error::Session("Session.call_stream".into())).err()
    }

    /// 流式调用，一次返回
    async fn stream_call(
        &mut self,
        _input: Box<dyn Stream<Item = Msg> + Send + Sync>,
    ) -> anyhow::Result<Msg> {
        anyhow::Error::from(Error::Session("Session.stream_call".into())).err()
    }

    /// 双向流式调用
    async fn stream(
        &mut self,
        _input: Box<dyn Stream<Item = Msg> + Send + Sync>,
    ) -> anyhow::Result<Box<dyn Stream<Item = Msg> + Send + Sync>> {
        anyhow::Error::from(Error::Session("Session.stream".into())).err()
    }

    /// 终止
    async fn abort(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

// ----------------------  会话元数据 -----------------------------

pub trait SessionMetadata: Debug {
    fn id(&self) -> &str;
    fn user_id(&self) -> &str;
    /// 会话提示词
    fn additional_tips(&self) -> Option<String> {
        None
    }
    /// 会话扩展信息
    fn extend(&self) -> Option<HashMap<String, String>> {
        None
    }
}

pub trait ErasedSessionMetadata: SessionMetadata {
    fn as_any(&self) -> &dyn Any;

    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T> ErasedSessionMetadata for T
where
    T: SessionMetadata + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// 会话元数据，用于传递会话相关信息
#[derive(Debug)]
pub struct SessionMD {
    /// 任意类型元数据，用于扩展
    pub inner: Box<dyn ErasedSessionMetadata + Send + Sync + 'static>,
}

impl SessionMD {
    pub fn new<T: SessionMetadata + Send + Sync + 'static>(meta: T) -> Self {
        Self {
            inner: Box::new(meta),
        }
    }

    pub fn get_session_id(&self) -> &str {
        self.inner.id()
    }
    pub fn get_user_id(&self) -> &str {
        self.inner.user_id()
    }

    pub fn into_inner<T>(self) -> Result<T, Self>
    where
        T: SessionMetadata + Send + 'static,
    {
        if self.inner.as_any().is::<T>() {
            let any = self.inner.into_any();

            let boxed = any
                .downcast::<T>()
                .expect("type was checked by as_any().is::<T>()");

            Ok(*boxed)
        } else {
            Err(self)
        }
    }
}

impl<T: SessionMetadata + Send + Sync + 'static> From<T> for SessionMD {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
// ----------------------  会话管理 -----------------------------

/// 会话管理 trait，定义会话管理的接口
#[async_trait::async_trait]
pub trait SessionCtl: Sync {
    // 加载session列表
    async fn list(
        &self,
        user_id: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<SessionMD>>;
    // 加载session详情
    async fn load(&self, user_id: &str, session_id: &str) -> anyhow::Result<Option<SessionMD>>;
    // 更改session
    async fn update(&self, meta: SessionMD) -> anyhow::Result<()>;
    // 创建session
    async fn create(&self, meta: SessionMD) -> anyhow::Result<()>;
    // 删除session
    async fn delete(&self, user_id: &str, session_id: &str) -> anyhow::Result<()>;
}
