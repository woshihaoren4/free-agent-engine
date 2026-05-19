use crate::{Task, TaskResult, TaskType};
use std::any::Any;
use std::ops::Deref;
use std::sync::Arc;
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
impl EnvEvent {
    pub fn is_none(&self) -> bool {
        if let Self::None = *self { true } else { false }
    }
    pub fn is_task(&self) -> bool {
        if let Self::TaskResult(_) = *self { true } else { false }
    }
}

/// 事物选择器，用于查询环境中的事物
#[derive(Default, Debug, PartialEq, Eq, Ord, PartialOrd, Hash, Clone)]
pub enum ThingSelect {
    /// 无选择器
    #[default]
    None,
    /// 环境变量
    EnvVar(String),
    /// 任务执行器：任务类型,渠道
    Executor(TaskType, String),
    /// plan 执行计划，PlanID,AgentID
    Plan(String, String),
    /// Custom
    Custom(String),
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
        Self {
            source,
            items: Vec::new(),
        }
    }
    pub fn add_item<T: Into<ThingItem>>(&mut self, item: T) -> &mut Self {
        self.items.push(item.into());
        self
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
pub enum ThingItem {
    #[default]
    None,
    /// 任务执行器,执行器描述
    Executor(String),
    /// plan 执行计划，计划ID
    Plan(String),
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
    async fn register_parent_env(&mut self, env: Env);

    /// 监听环境事件，返回事件通道
    /// 注意：事件是不是所有消费者共享的，A消费了这个事件则B不会收到这个事件
    async fn watch(&self) -> anyhow::Result<EnvEvent>;

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
