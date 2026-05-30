mod session_event_layer;
mod session_trait_ext;
pub mod file_session_ctl;
mod session_ext;

pub use session_event_layer::*;
pub use session_trait_ext::*;
pub use session_ext::*;
use std::any::Any;



use crate::Msg;
use crate::define::Message;
use crate::error::Error;
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

// ----------------------  会话管理 -----------------------------

/// 会话元数据，用于传递会话相关信息
#[derive(Debug)]
pub struct SessionMetadata {
    /// 会话ID
    pub session_id: String,
    /// 用户ID
    pub user_id: String,
    /// 任意类型元数据，用于扩展
    pub data: Box<dyn Any + Send + Sync + 'static>,
}

pub struct SessionMD<T> {
    /// 会话ID
    pub session_id: String,
    /// 用户ID
    pub user_id: String,
    /// 会话数据
    pub data: T,
}

impl Default for SessionMetadata {
    fn default() -> Self {
        Self {
            session_id: wd_tools::uuid::v4(),
            user_id: "master".to_string(),
            data: Box::new(()),
        }
    }
}

impl SessionMetadata {
    pub fn with_session_id<S: Into<String>>(session_id: S) -> Self {
        Self {
            session_id: session_id.into(),
            user_id: "master".to_string(),
            data: Box::new(()),
        }
    }
    pub fn set_session_id<S: Into<String>>(mut self, session_id: S) -> Self {
        self.session_id = session_id.into();
        self
    }
    pub fn get_session_id(&self) -> &str {
        self.session_id.as_str()
    }
    pub fn get_user_id(&self) -> &str {
        self.user_id.as_str()
    }
    pub fn set_user_id<S: Into<String>>(mut self, user_id: S) -> Self {
        self.user_id = user_id.into();
        self
    }
    pub fn set_data<T: Any + Send + Sync + 'static>(mut self, data: T) -> Self {
        self.data = Box::new(data);
        self
    }
    pub fn try_to_session_md<T: Any>(mut self) -> Result<SessionMD<T>, SessionMetadata> {
        match self.data.downcast::<T>() {
            Ok(t) => Ok(SessionMD {
                session_id: self.session_id,
                user_id: self.user_id,
                data: *t,
            }),
            Err(e) => {
                self.data = e;
                Err(self)
            }
        }
    }
}

impl<T: ToString> From<T> for SessionMetadata {
    fn from(value: T) -> Self {
        Self::with_session_id(value.to_string())
    }
}
// ----------------------  会话管理 -----------------------------

/// 会话管理 trait，定义会话管理的接口
#[async_trait::async_trait]
pub trait SessionCtl: Sync {
    // 加载session列表
    async fn list(&self, user_id: &str, offset: usize, limit: usize) -> anyhow::Result<Vec<SessionMetadata>>;
    // 加载session详情
    async fn load(&self, user_id: &str, session_id: &str) -> anyhow::Result<Option<SessionMetadata>>;
    // 更改session
    async fn update(&self, user_id: &str, session_id: &str, meta: SessionMetadata) -> anyhow::Result<()>;
    // 创建session
    async fn create(&self, user_id: &str, meta: SessionMetadata) -> anyhow::Result<()>;
    // 删除session
    async fn delete(&self, user_id: &str, session_id: &str) -> anyhow::Result<()>;
}

// ----------------------  解析会话元数据 -----------------------------

impl<T: Any> SessionMD<T> {
    pub fn new(session_id: SessionMetadata, data: T) -> Self {
        Self {
            session_id: session_id.session_id,
            user_id: session_id.user_id,
            data,
        }
    }
    pub fn get_user_id(&self) -> &str {
        self.user_id.as_str()
    }
    pub fn get_session_id(&self) -> &str {
        self.session_id.as_str()
    }
    pub fn get_data(&self) -> &T {
        &self.data
    }
    pub fn get_data_mut(&mut self) -> &mut T {
        &mut self.data
    }
    pub fn into_data(self) -> T {
        self.data
    }
}