use std::any::Any;
use std::ops::Deref;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_stream::Stream;
use wd_tools::channel::Channel;
use wd_tools::PFErr;
use crate::{Error, TaskType};
use crate::task::{Task, TaskResult};

/// 环境事件类型，用于表示环境中发生的各种事件
#[derive(Default, Debug)]
pub enum EnvEvent {
    /// 无事件
    #[default]
    None,
    // 事件执行结果，
    TaskResult(TaskResult),
    // /// 键值对事件，携带键和值
    // KV(String, Value),
    // /// 自定义事件，携带自定义信息
    // Custom(String),
    // /// 任意类型事件，用于扩展
    // Any(Box<dyn Any + Send + Sync + 'static>),
}

/// 事物选择器，用于查询环境中的事物
#[derive(Default, Debug, PartialEq, Eq, Ord, PartialOrd, Hash, Clone)]
pub enum ThingSelect {
    /// 无选择器
    #[default]
    None,
    /// 环境变量
    EnvVar(String),
    /// 任务执行器
    Executor(TaskType),
}
impl ThingSelect {
    pub fn is_none(&self) -> bool {
        self == &ThingSelect::None
    }
}

/// 事物类型，表示环境中的各种对象
#[derive(Default, Debug)]
pub struct Thing {
    pub source: String,
    pub items: Vec<ThingItem>,
}
impl Thing {
    pub fn new(source: String) -> Self {
        Self { source, items: Vec::new() }
    }
    pub fn add_item<T:Into<ThingItem>>(&mut self, item: T)->&mut Self {
        self.items.push(item.into());self
    }
    pub fn set_items(&mut self, items: Vec<ThingItem>) -> &mut Self {
        self.items = items;
        self
    }
    pub fn into_self(&mut self) -> Self {
        let source = std::mem::take(&mut self.source);
        let items = std::mem::take(&mut self.items);
        Self { source, items }
    }
}
#[derive(Default, Debug)]
pub enum ThingItem{
    #[default]
    None,
    /// 任务执行器,执行器描述
    Executor(String),
    /// 模块
    Module(String),
    /// 工具
    Tool(String),
    /// 智能体
    Agent(String),
    /// 技能
    Skill(String),
    /// 自定义事物
    Custom(String),
    /// MCP服务器
    McpServer(String),
    /// 信息
    Info(String),
    /// 任意类型事物，用于扩展
    Any(Box<dyn Any + Send + Sync + 'static>),
}


/// 环境 trait，定义智能体运行的环境接口
#[async_trait::async_trait]
pub trait Environment: Send + Sync + 'static {
    /// 当前环境的唯一标识
    fn id(&self) -> &'static str;

    /// 父子环境嵌套
    async fn register_parent_env(&mut self, env:Env);

    /// 监听环境事件，返回事件通道
    /// 注意：事件是不是所有消费者共享的，A消费了这个事件则B不会收到这个事件
    async fn watch(&self) -> EnvEvent;

    /// 查询环境中的事物
    async fn query(&self, select: ThingSelect) -> anyhow::Result<Vec<Thing>>;
    
    /// 异步执行任务
    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()>;

    /// 同步执行任务
    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult>;
}

/// 环境封装，提供线程安全的环境访问
#[derive(Clone)]
pub struct Env(Arc<dyn Environment + Send + 'static>);
impl From<Arc<dyn Environment + Send + 'static>> for Env {
    fn from(env: Arc<dyn Environment + Send + 'static>) -> Self {
        Self(env)
    }
}
impl Env {
    /// 创建新的环境封装
    pub fn new(env: impl Environment) -> Self {
        Self(Arc::new(env))
    }
    /// 获取内部环境引用
    pub fn inner(&self) -> Arc<dyn Environment> {
        self.0.clone()
    }
}
impl Deref for Env {
    type Target = Arc<dyn Environment + Send + 'static>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

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