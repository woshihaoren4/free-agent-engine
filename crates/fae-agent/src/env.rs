use crate::{AgentTask, AgentTaskStatus, McpToolRequest, SkillHeader, Task, TaskResult, TaskType};
use serde_json::Value;
use std::any::Any;
use std::collections::HashMap;
use std::env;
use std::fmt::{Debug, Display};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use wd_tools::PFArc;

pub const FAE_HOME: &'static str = "FAE_HOME";
pub const FAE_WORKSPACE: &'static str = "FAE_WORKSPACE";
pub const OPENAI_DEFAULT_MODEL: &'static str = "OPENAI_DEFAULT_MODEL";
pub const FAE_DEFAULT_MODEL: &'static str = "FAE_DEFAULT_MODEL";

pub fn fae_home() -> PathBuf {
    if let Ok(o) = env::var("FAE_HOME") {
        PathBuf::from(o)
    } else {
        let home_dir = dirs::home_dir().expect("Failed to get home directory");
        let fae_dir = home_dir.join(".fae");
        fae_dir
    }
}

#[derive(Default, Debug, PartialEq, Eq, Clone)]
pub struct TimedTask {
    pub task_content: String,
    pub agent_id: String,
    pub user_id: String,
    pub session_id: String,
}

/// 环境事件类型，用于表示环境中发生的各种事件
#[derive(Default, Debug)]
pub enum EnvEvent {
    /// 无事件
    #[default]
    None,
    // 事件执行结果，
    TaskResult(TaskResult),
    // 定时事件
    Timed(TimedTask),
    // 心跳
    Heartbeat(String),
    // agent 事件
    Agent(AgentTask),
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
        if let Self::TaskResult(_) = *self {
            true
        } else {
            false
        }
    }
}

/// 事物选择器，用于查询环境中的事物
#[derive(Default, Debug, PartialEq, Eq, Ord, PartialOrd, Hash, Clone)]
pub enum ThingSelect {
    /// 无选择器
    #[default]
    None,
    /// 环境变量
    Env(String),
    /// 任务执行器：任务类型,渠道
    Executor(TaskType, String),
    /// plan 执行计划，PlanID,AgentID
    Plan(String, String),
    /// tools 工具: 渠道，工具名称
    Tool(String, String),
    /// MCP服务,渠道，mcp名称
    Mcp(String, String),
    /// skill: 渠道，名字, 目录
    Skill(String, String, Option<String>),
    /// agent 任务记录，任务ID
    AgenTask(String),
    /// Custom
    Custom(String),
}
impl ThingSelect {
    pub fn is_none(&self) -> bool {
        self == &ThingSelect::None
    }
}

/// 查询条件
#[derive(Default, Debug, PartialEq, Eq, Clone)]
pub struct Select {
    pub select: ThingSelect,
    pub workspace: Option<String>,
    pub extend: HashMap<String, String>,
}

impl From<ThingSelect> for Select {
    fn from(select: ThingSelect) -> Self {
        Self {
            select,
            workspace: None,
            extend: HashMap::new(),
        }
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
    /// 工具:工具描述,工具参数
    Tool(String, Value),
    /// 环境变量
    EnvVar(String),
    /// 智能体
    Agent(String),
    /// agent 任务记录，任务ID
    AgenTask(Vec<AgentTaskStatus>),
    /// 技能: Skill头信息
    Skill(SkillHeader),
    /// 自定义事物
    Custom(String),
    /// MCP服务器
    Mcp(Vec<McpToolRequest>),
    /// 信息
    Info(String),
    /// 任意类型事物，用于扩展
    Any(Box<dyn Any + Send + Sync + 'static>),
}
impl ThingItem {
    pub fn string(&self) -> String {
        match self {
            Self::None => "".to_string(),
            Self::Executor(s) => s.to_string(),
            Self::Plan(s) => s.to_string(),
            Self::Module(s) => s.to_string(),
            Self::Tool(s, _) => s.to_string(),
            Self::EnvVar(s) => s.to_string(),
            Self::Agent(s) => s.to_string(),
            Self::AgenTask(s) => serde_json::to_string(s).unwrap_or("ThingItem::AgenTask".into()),
            Self::Skill(s) => s.to_string(),
            Self::Custom(s) => s.to_string(),
            Self::Mcp(s) => serde_json::to_string(s).unwrap_or("ThingItem::McpServer".into()),
            Self::Info(s) => s.to_string(),
            Self::Any(_) => "".to_string(),
        }
    }
}

/// 环境 trait，定义智能体运行的环境接口
#[async_trait::async_trait]
pub trait Environment: Debug + Send + Sync + 'static {
    /// 当前环境的唯一标识
    fn id(&self) -> &'static str;

    /// 父子环境嵌套
    async fn register_parent_env(&mut self, env: Env);

    /// 监听环境事件，返回事件通道
    /// 注意：事件是不是所有消费者共享的，A消费了这个事件则B不会收到这个事件
    async fn watch(&self) -> anyhow::Result<EnvEvent>;

    /// 查询环境中的事物
    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>>;

    /// 异步执行任务
    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()>;

    /// 同步执行任务
    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult>;
}

/// 环境封装，提供线程安全的环境访问
#[derive(Clone, Debug)]
pub struct Env(Arc<dyn Environment + Send + 'static>);
impl From<Arc<dyn Environment + Send + 'static>> for Env {
    fn from(env: Arc<dyn Environment + Send + 'static>) -> Self {
        Self(env)
    }
}
impl<T> From<T> for Env
where
    T: Environment + Send + 'static,
{
    fn from(env: T) -> Self {
        Self(Arc::new(env))
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

    pub fn none() -> Env {
        Self::new(NoneEnv::default())
    }
}
impl Deref for Env {
    type Target = Arc<dyn Environment + Send + 'static>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// ----------- 默认带一个空的实现 ----------------------
#[derive(Default, Debug)]
pub struct NoneEnv(Option<Env>);

#[async_trait::async_trait]
impl Environment for NoneEnv {
    fn id(&self) -> &'static str {
        ""
    }

    async fn register_parent_env(&mut self, _env: Env) {
        self.0 = Some(_env);
    }

    async fn watch(&self) -> anyhow::Result<EnvEvent> {
        if let Some(env) = &self.0 {
            return env.watch().await;
        } else {
            return Err(anyhow::anyhow!("NoneEnv watch failed!"));
        }
    }

    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>> {
        if let Some(env) = &self.0 {
            return env.query(select).await;
        } else {
            return Err(anyhow::anyhow!("NoneEnv query failed!"));
        }
    }

    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()> {
        if let Some(env) = &self.0 {
            return env.spawn(tasks).await;
        } else {
            return Err(anyhow::anyhow!("NoneEnv spawn failed!"));
        }
        Ok(())
    }

    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult> {
        if let Some(env) = &self.0 {
            return env.execute(task).await;
        } else {
            return Err(anyhow::anyhow!("NoneEnv execute failed!"));
        }
    }
}
