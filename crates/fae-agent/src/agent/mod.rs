pub mod single_agent;
pub use single_agent::*;

use crate::session::{Session};
use crate::{Env, EnvEvent, TaskResult};
use std::any::Any;
use std::ops::Deref;
use std::sync::Arc;

/// 命令类型，表示系统和用户命令
#[derive(Default, Debug)]
pub enum Command {
    /// 无命令
    #[default]
    None,
    /// 系统退出命令, /exit
    SystemExit,
    /// 自定义命令
    CustomCommand(String),
}
impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Command::None, Command::None) => true,
            (Command::SystemExit, Command::SystemExit) => true,
            (Command::CustomCommand(a), Command::CustomCommand(b)) => a == b,
            _ => false,
        }
    }
}



#[derive(Debug)]
pub struct SessionMetaUser {
    pub user_id: String,
}

/// 会话元数据，用于传递会话相关信息
#[derive(Debug)]
pub struct SessionMetadata {
    /// 会话ID
    pub session_id: String,
    /// 任意类型元数据，用于扩展
    pub data: Box<dyn Any + Send + Sync + 'static>,
}

pub struct SessionMD<T>{
    /// 会话ID
    pub session_id: String,
    /// 会话数据
    pub data: T,
}

impl Default for SessionMetadata {
    fn default() -> Self {
        Self {
            session_id: wd_tools::uuid::v4(),
            data: Box::new(()),
        }
    }
}

impl SessionMetadata {
    pub fn set_session_id<S:Into<String>>(mut self, session_id: S)->Self {
        self.session_id = session_id.into();self
    }
    pub fn get_session_id(&self) -> &str {
        self.session_id.as_str()
    }
    pub fn set_data<T:Any+Send+Sync+'static>(mut self, data: T)->Self {
        self.data = Box::new(data);
        self
    }
    pub fn try_to_session_md<T:Any>(mut self) -> Result<SessionMD<T>, SessionMetadata> {
        match self.data.downcast::<T>() {
            Ok(t) => {
                Ok(SessionMD {
                    session_id: self.session_id,
                    data: *t,
                })
            }
            Err(e) => {
                self.data = e;
                Err(self)
            }
        }
    }
}

impl<T:Any> SessionMD<T> {
    pub fn get_session_id(&self) -> &str {
        self.session_id.as_str()
    }
    pub fn get_data(&self) -> &T{
        &self.data
    }
    pub fn get_data_mut(&mut self) -> &mut T{
        &mut self.data
    }
}

/// 智能体 trait，定义智能体的核心接口
#[async_trait::async_trait]
pub trait Agent: Sync {
    /// 智能体ID
    fn id(&self) -> String;

    /// 处理环境事件
    async fn on_env(&self, env: Env, event: EnvEvent) -> anyhow::Result<()>;

    /// 处理会话请求
    async fn on_session(
        &self,
        env: Env,
        meta: SessionMetadata,
    ) -> anyhow::Result<Box<dyn Session + Send + 'static>>;

    /// 处理命令
    async fn on_command(&self, env: Env, cmd: Command) -> anyhow::Result<()>;

    /// 退出
    async fn exit(&self) {}
}

#[derive(Clone)]
pub struct AgentRef(Arc<dyn Agent + Send + 'static>);
impl Deref for AgentRef {
    type Target = Arc<dyn Agent + Send + 'static>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<T> From<T> for AgentRef
where
    T: Agent + Send + 'static,
{
    fn from(agent: T) -> Self {
        Self(Arc::new(agent))
    }
}
