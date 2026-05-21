pub mod single_agent;
pub use single_agent::*;

use crate::session::{Message, Session};
use crate::task::Task;
use crate::{Env, EnvEvent, Error, TaskResult};
use std::any::Any;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::Stream;
use wd_tools::channel::{ChannelResult, Sender};

/// 命令类型，表示系统和用户命令
#[derive(Default, Debug)]
pub enum Command {
    /// 无命令
    #[default]
    None,
    /// 系统重置命令
    SystemReset,
    /// 系统退出命令
    SystemExit,
    /// 用户自定义命令
    UserCustomCommand(String),
    /// 任意类型命令，用于扩展
    Any(Box<dyn Any + Send + Sync + 'static>),
}
impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Command::None, Command::None) => true,
            (Command::SystemReset, Command::SystemReset) => true,
            (Command::SystemExit, Command::SystemExit) => true,
            (Command::UserCustomCommand(a), Command::UserCustomCommand(b)) => a == b,
            (Command::Any(_), Command::Any(_)) => false, // 无法比较Any类型
            _ => false,
        }
    }
}

/// 事件类型，表示系统中发生的各种事件
#[derive(Default)]
pub enum Event {
    /// 无事件
    #[default]
    None,
    /// session事件
    SessionCall(SessionInfo, Message),
    SessionCallStream(SessionInfo, Message, Sender<Message>),
    SessionStreamCall(
        SessionInfo,
        Box<dyn Stream<Item = Message> + Send + Sync + 'static>,
    ),
    SessionStream(
        SessionInfo,
        Box<dyn Stream<Item = Message> + Send + Sync + 'static>,
        Sender<Message>,
    ),
    /// 环境事件
    EnvEvent(EnvEvent),
    /// 任务完成事件
    TaskOver(TaskResult),
    /// 命令
    Command(Command),
}

#[derive(Debug)]
pub struct SenderMessageStream<T> {
    sender: Sender<Message>,
    inner: PhantomData<T>,
}

impl<T: Any + Send + Sync + 'static> SenderMessageStream<T> {
    pub async fn send(&self, id: &str, message: T) -> anyhow::Result<()> {
        if let Err(e) = self
            .sender
            .send(Message::new(id).set_content(message))
            .await
        {
            return Err(anyhow::anyhow!(
                "[SenderMessageStream] send message error: {:?}",
                e
            ));
        }
        Ok(())
    }
    pub fn close(&self) {
        self.sender.close();
    }
}
impl Event {
    pub fn sender_message_to_stream_t<M>(sender: Sender<Message>) -> SenderMessageStream<M> {
        SenderMessageStream {
            sender,
            inner: PhantomData::<M>::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionMetaUser {
    pub user_id: String,
}

/// 会话元数据，用于传递会话相关信息
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// 会话ID
    pub session_id: String,
    /// 使用者信息
    pub user: SessionMetaUser,
    /// 任意类型元数据，用于扩展
    pub extend_any: Option<Arc<dyn Any + Send + Sync + 'static>>,
}

impl Default for SessionInfo {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            user: SessionMetaUser {
                user_id: String::new(),
            },
            extend_any: None,
        }
    }
}

impl SessionInfo {
    pub fn get_session_id(&self) -> &str {
        self.session_id.as_str()
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
        meta: SessionInfo,
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
