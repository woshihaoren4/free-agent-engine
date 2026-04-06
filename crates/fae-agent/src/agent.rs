use std::any::Any;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_stream::Stream;
use wd_tools::channel::Channel;
use wd_tools::PFErr;
use crate::Error;

/// 环境事件类型，用于表示环境中发生的各种事件
#[derive(Default, Debug)]
pub enum EnvEvent {
    /// 无事件
    #[default]
    None,
    /// 心跳事件，携带心跳信息
    Heartbeat(String),
    /// 键值对事件，携带键和值
    KV(String, Value),
    /// 自定义事件，携带自定义信息
    Custom(String),
    /// 任意类型事件，用于扩展
    Any(Box<dyn Any + Send + Sync + 'static>),
}

/// 环境监控 trait，用于监听环境事件
#[async_trait::async_trait]
pub trait EnvironmentWatch: Sync {
    /// 监听环境事件，返回事件通道
    async fn watch(&self) -> Channel<EnvEvent>;
}
pub struct EnvWatch(Arc<dyn EnvironmentWatch + Send + 'static>);

/// 事物选择器，用于查询环境中的事物
#[derive(Default, Debug, Clone)]
pub struct ThingSelect {
    /// 查询语句
    pub query: String,
}

impl ThingSelect {
    /// 创建新的事物选择器
    pub fn new(query: impl Into<String>) -> Self {
        Self { query: query.into() }
    }
}

/// 事物类型，表示环境中的各种对象
#[derive(Default, Debug)]
pub enum Thing {
    /// 无事物
    #[default]
    None,
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
pub trait EnvironmentHandler: Send + Sync + 'static {
    /// 查询环境中的事物
    async fn query(&self, select: ThingSelect) -> anyhow::Result<Vec<Thing>>;
    
    /// 执行任务
    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()>;

    /// 等待任务完成
    async fn do_wait(&self, task: Task) -> anyhow::Result<TaskResult>;
}

/// 环境封装，提供线程安全的环境访问
#[derive(Clone)]
pub struct Env(Arc<dyn EnvironmentHandler + Send + 'static>);
impl From<Arc<dyn EnvironmentHandler + Send + 'static>> for Env {
    fn from(env: Arc<dyn EnvironmentHandler + Send + 'static>) -> Self {
        Self(env)
    }
}

impl Env {
    /// 创建新的环境封装
    pub fn new(env: impl EnvironmentHandler) -> Self {
        Self(Arc::new(env))
    }
    
    /// 获取内部环境引用
    pub fn inner(&self) -> Arc<dyn EnvironmentHandler> {
        self.0.clone()
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

/// 任务类型，表示智能体需要执行的任务
#[derive(Default, Debug)]
pub enum Task {
    /// 无任务
    #[default]
    None,
    /// 执行模块
    Module(String),
    /// 执行工具
    Tool(String),
    /// 执行智能体
    Agent(String),
    /// 执行技能
    Skill(String),
    /// 执行自定义任务
    Custom(String),
    /// 输出结果
    Output(String),
    /// 错误信息
    Error(String),
    /// 任务完成
    Over,
    /// 任意类型任务，用于扩展
    Any(Box<dyn Any + Send + Sync + 'static>),
}
impl PartialEq for Task {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Task::None, Task::None) => true,
            (Task::Module(a), Task::Module(b)) => a == b,
            (Task::Tool(a), Task::Tool(b)) => a == b,
            (Task::Agent(a), Task::Agent(b)) => a == b,
            (Task::Skill(a), Task::Skill(b)) => a == b,
            (Task::Custom(a), Task::Custom(b)) => a == b,
            (Task::Output(a), Task::Output(b)) => a == b,
            (Task::Error(a), Task::Error(b)) => a == b,
            (Task::Over, Task::Over) => true,
            (Task::Any(_), Task::Any(_)) => false, // 无法比较Any类型
            _ => false,
        }
    }
}
#[derive(Default, Debug)]
pub struct  TaskResult{
    pub result: String,
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
    use super::*;

    #[test]
    fn test_enum_derives() {
        // 测试枚举的派生特性
        let msg1 = Message::Text("hello".to_string());
        let msg2 = Message::Text("hello".to_string());
        assert_eq!(msg1, msg2);
        
        let task1 = Task::Tool("test".to_string());
        let task2 = Task::Tool("test".to_string());
        assert_eq!(task1, task2);
    }
    
    #[test]
    fn test_thing_select() {
        // 测试事物选择器
        let select = ThingSelect::new("test query");
        assert_eq!(select.query, "test query");
    }
    
    #[tokio::test]
    async fn test_env_creation() {
        // 测试环境封装的创建
        struct TestEnv;
        #[async_trait::async_trait]
        impl EnvironmentHandler for TestEnv {
            async fn query(&self, _select: ThingSelect) -> anyhow::Result<Vec<Thing>> {
                Ok(vec![])
            }
            async fn spawn(&self, _tasks: Vec<Task>) -> anyhow::Result<()> {
                Ok(())
            }

            async fn do_wait(&self, _task: Task) -> anyhow::Result<TaskResult> {
                anyhow::anyhow!("todo").err()
            }
        }
        
        let env = Env::new(TestEnv);
        assert!(env.inner().query(ThingSelect::default()).await.is_ok());
    }
}