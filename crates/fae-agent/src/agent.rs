use std::any::Any;
use std::ops::Deref;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_stream::Stream;
use wd_tools::channel::Channel;
use wd_tools::PFErr;
use crate::{Env, EnvEvent, Error, TaskType};
use crate::task::{Task, TaskResult};

/// 消息类型，表示智能体间的通信内容
#[derive(Default, Debug)]
pub enum Message {
    /// 无消息
    #[default]
    None,
    /// 文本消息
    Text(String),
    /// 二进制消息
    Binary(Vec<u8>),
    /// JSON值消息
    Value(Value),
    /// 命令消息
    Command(Command),
    /// 任意类型消息，用于扩展
    Any(Box<dyn Any + Send + Sync + 'static>),
}

impl PartialEq for Message {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Message::None, Message::None) => true,
            (Message::Text(a), Message::Text(b)) => a == b,
            (Message::Binary(a), Message::Binary(b)) => a == b,
            (Message::Value(a), Message::Value(b)) => a == b,
            (Message::Command(a), Message::Command(b)) => a == b,
            (Message::Any(_), Message::Any(_)) => false, // 无法比较Any类型
            _ => false,
        }
    }
}

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

/// 会话 trait，定义智能体与外部交互的接口
#[async_trait::async_trait]
pub trait Session: Sync{
    /// 同步调用，返回单个消息
    async fn call(&self, input: Message) -> anyhow::Result<Message, Error> {
        Error::NoSupport("Session.call".into()).err()
    }

    /// 调用并返回流
    async fn call_stream(&self, input: Message) -> anyhow::Result<Box<dyn Stream<Item = Message> + Send>, Error> {
        Error::NoSupport("Session.call_stream".into()).err()
    }

    /// 流式调用，返回多个消息
    async fn stream_call(&self, input: Box<dyn Stream<Item = Message> + Send>) -> anyhow::Result<Vec<Message>, Error> {
        Error::NoSupport("Session.stream_call".into()).err()
    }

    /// 双向流式调用
    async fn stream(&self, input: Box<dyn Stream<Item = Message> + Send>) -> anyhow::Result<Box<dyn Stream<Item = Message> + Send>, Error> {
        Error::NoSupport("Session.stream".into()).err()
    }
}

/// 事件类型，表示系统中发生的各种事件
#[derive(Default, Debug)]
pub enum Event {
    /// 无事件
    #[default]
    None,
    /// 会话事件，携带消息
    Session(Message),
    /// 环境事件
    EnvEvent(EnvEvent),
    /// 任务完成事件
    TaskOver(Task)
}


/// 上下文，用于存储智能体执行过程中的状态
#[derive(Debug, Clone)]
pub struct Context {
    inner: Arc<Mutex<Box<dyn Any + Send + Sync + 'static>>>,
}

impl Context {
    /// 创建新的上下文
    pub fn new<T: Any + Send + Sync + 'static>(data: T) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Box::new(data))),
        }
    }
}

/// 会话元数据，用于传递会话相关信息
#[derive(Debug)]
pub enum SessionMeta {
    /// 任意类型元数据，用于扩展
    Any(Box<dyn Any + Send + Sync + 'static>),
}

/// 智能体规划 trait，定义智能体的规划逻辑
#[async_trait::async_trait]
pub trait AgentPlanning: Send + Sync + 'static {
    /// 开始规划
    async fn start(&self, env: Env, event: &Event) -> anyhow::Result<Context>;
    
    /// 下一步规划
    async fn next_step(&self, env: Env, ctx: &mut Context, event: Event) -> anyhow::Result<Vec<Task>>;
    
    /// 规划完成
    async fn over(&self, env: Env, ctx: &mut Context) -> anyhow::Result<()>;
}

/// 智能体 trait，定义智能体的核心接口
#[async_trait::async_trait]
pub trait Agent: Send + Sync + 'static {
    /// 处理环境事件
    async fn on_env(&self, env: Env, event: EnvEvent) -> anyhow::Result<()>;
    
    /// 处理会话请求
    async fn on_session(&self, env: Env, meta: SessionMeta) -> anyhow::Result<Box<dyn Session + Send + 'static>>;
    
    /// 处理命令
    async fn on_command(&self, env: Env, cmd: Command) -> anyhow::Result<()>;
}

pub struct AgentRef(Arc<dyn Agent + Send + Sync + 'static>);

#[cfg(test)]
mod tests {

}