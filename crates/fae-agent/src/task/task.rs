use crate::{Context, EndPlanTaskArgs, Env, Planning, ToolRequest};
use async_openai::types::chat::CreateChatCompletionRequest;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::any::Any;
use std::any::TypeId;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};

pub const GLOBAL_KEY_AGENT_ID: &str = "AGENT_ID";
pub const GLOBAL_KEY_PLAN_ID: &str = "PLAN_ID";
pub const GLOBAL_KEY_SESSION_ID: &str = "SESSION_ID";
pub const GLOBAL_KEY_WORKSPACE: &str = "WORKSPACE";
pub const GLOBAL_KEY_PROJECT: &str = "PROJECT";
pub const GLOBAL_KEY_PROJECT_DIR: &str = "PROJECT_DIR";

pub type RawTaskArgs = Box<dyn Any + Send + Sync + 'static>;

#[derive(Debug)]
pub enum PlanTaskArgs {
    Planning(Box<dyn Planning + Send + 'static>),
    Abort(EndPlanTaskArgs),
}

impl From<Box<dyn Planning + Send + 'static>> for PlanTaskArgs {
    fn from(value: Box<dyn Planning + Send + 'static>) -> Self {
        Self::Planning(value)
    }
}

impl From<EndPlanTaskArgs> for PlanTaskArgs {
    fn from(value: EndPlanTaskArgs) -> Self {
        Self::Abort(value)
    }
}


#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AgentRequest{
    pub agent_id: String,
    pub input: String,
    pub session_id: String,
    pub user_id: String,
}

#[derive(Default, Debug, PartialEq, Eq, Ord, PartialOrd, Hash, Clone)]
pub enum TkTy {
    /// 无任务
    #[default]
    None,
    /// 执行模型
    Model,
    /// 执行一个计划
    Plan,
    /// 执行工具
    Tool,
    /// 执行智能体
    Agent,
    /// 执行技能
    Skill,
    /// 执行MCP服务器
    Mcp,
    /// 执行自定义任务
    Custom,
    /// 输出结果
    Output,
    /// 错误信息
    Error,
    /// 任务完成
    Over,
    /// 任意类型
    Any(String),
}
impl Display for TkTy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TkTy::None => "none",
            TkTy::Model => "model",
            TkTy::Plan => "plan",
            TkTy::Tool => "tools",
            TkTy::Agent => "agent",
            TkTy::Skill => "skill",
            TkTy::Mcp => "mcp",
            TkTy::Custom => "custom",
            TkTy::Output => "output",
            TkTy::Error => "error",
            TkTy::Over => "over",
            TkTy::Any(s) => s    .as_str(),
        };
        write!(f, "{}", s)
    }
}
/// 任务类型，表示智能体需要执行的任务
#[derive(Debug)]
pub enum TaskReq {
    /// 无任务
    None,
    /// 执行模型
    Model(CreateChatCompletionRequest),
    /// 执行一个计划
    Plan(PlanTaskArgs),
    /// 执行工具
    Tool(ToolRequest),
    /// 执行智能体
    Agent(AgentRequest),
    /// 执行技能
    Skill,
    /// 执行MCP服务器
    Mcp(ToolRequest),
    /// 执行自定义任务
    Custom(RawTaskArgs),
    /// 输出结果
    Output,
    /// 错误信息
    Error,
    /// 任务完成
    Over,
    /// 任意类型
    Any(String, Option<RawTaskArgs>),
}
impl Default for TaskReq {
    fn default() -> Self {
        Self::None
    }
}

impl TaskReq {
    pub fn model(args: impl Into<CreateChatCompletionRequest>) -> Self {
        Self::Model(args.into())
    }
    pub fn plan(args: impl Into<PlanTaskArgs>) -> Self {
        Self::Plan(args.into())
    }
    pub fn tool(args: ToolRequest) -> Self {
        Self::Tool(args)
    }
    pub fn mcp(args: ToolRequest) -> Self {
        Self::Mcp(args)
    }
    fn kind(&self) -> TkTy {
        match self {
            TaskReq::None => TkTy::None,
            TaskReq::Model(_) => TkTy::Model,
            TaskReq::Plan(_) => TkTy::Plan,
            TaskReq::Tool(_) => TkTy::Tool,
            TaskReq::Agent(_) => TkTy::Agent,
            TaskReq::Skill => TkTy::Skill,
            TaskReq::Mcp(_) => TkTy::Mcp,
            TaskReq::Custom(_) => TkTy::Custom,
            TaskReq::Output => TkTy::Output,
            TaskReq::Error => TkTy::Error,
            TaskReq::Over => TkTy::Over,
            TaskReq::Any(s, _) => TkTy::Any(s.into()),
        }
    }
}
impl PartialEq for TaskReq {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TaskReq::Any(a, _), TaskReq::Any(b, _)) => a == b,
            _ => self.kind() == other.kind(),
        }
    }
}
impl Eq for TaskReq {}
impl PartialOrd for TaskReq {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for TaskReq {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (TaskReq::Any(a, _), TaskReq::Any(b, _)) => a.cmp(b),
            _ => self.kind().cmp(&other.kind()),
        }
    }
}
impl Hash for TaskReq {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.kind().hash(state);
        if let TaskReq::Any(s, _) = self {
            s.hash(state);
        }
    }
}



/// 任务类型，表示智能体需要执行的任务
#[derive(Debug)]
pub struct Task {
    pub id: String,
    pub agent_id: String,
    pub req: TaskReq,
    pub exec_channel: String,
    pub user_id: String,
    pub ctx: Context,
    pub ext: Option<Box<dyn Any + Sync + Send + 'static>>,
}
impl Task {
    pub fn none() -> Self {
        Self::new(Context::new(Env::none()), "", "", TaskReq::None)
    }
    pub fn new<I: Into<String>, A: Into<String>>(
        ctx: Context,
        id: I,
        agent_id: A,
        req: TaskReq,
    ) -> Self {
        let id = id.into();
        let agent_id = agent_id.into();
        Self {
            id,
            agent_id,
            req,
            ctx,
            user_id: "".into(),
            exec_channel: "default".into(),
            ext: None,
        }
    }
    pub fn with_content(ctx: Context) -> Self {
        Self::new(ctx, wd_tools::uuid::v4(), "", TaskReq::None)
    }
    pub fn set_id<T: Into<String>>(mut self, id: T) -> Self {
        self.id = id.into();
        self
    }
    pub fn get_id(&self) -> &str {
        self.id.as_str()
    }
    pub fn get_agent_id(&self) -> &str {
        self.agent_id.as_str()
    }
    pub fn get_exec_channel(&self) -> &str {
        self.exec_channel.as_str()
    }
    pub fn set_type<T: Into<TaskReq>>(mut self, t: T) -> Self {
        self.req = t.into();
        self
    }
    pub fn get_type(&self) -> TkTy {
        self.req.kind()
    }
    pub fn remove_req(&mut self)->TaskReq {
        std::mem::replace(&mut self.req, TaskReq::None)
    }
    pub fn set_exec_channel<T: Into<String>>(mut self, t: T) -> Self {
        self.exec_channel = t.into();
        self
    }
    pub fn set_agent_id<T: Into<String>>(mut self, agent_id: T) -> Self {
        self.agent_id = agent_id.into();
        self
    }
    pub fn set_user_id<T: Into<String>>(mut self, user_id: T) -> Self {
        self.user_id = user_id.into();
        self
    }
    pub fn get_user_id(&self) -> &str {
        self.user_id.as_str()
    }
    pub fn set_channel<T: Into<String>>(mut self, channel: T) -> Self {
        self.exec_channel = channel.into();
        self
    }
    pub fn get_channel(&self) -> &str {
        self.exec_channel.as_str()
    }
    pub fn set_context(mut self, context: Context) -> Self {
        self.ctx = context;
        self
    }
    pub fn get_context(&self) -> Context {
        self.ctx.clone()
    }
    pub fn set<K: Into<String>, V: Into<String>>(&mut self, key: K, value: V) {
        self.ctx.set(key.into(), value.into());
    }
    pub fn get(&self, key: &str) -> Option<String> {
        self.ctx.get(key)
    }
    pub fn set_args_raw(mut self, args: Box<dyn Any + Send + Sync + 'static>) -> Self {
        self.ext = Some(args);self
    }
    pub fn set_ext<T: Any + Send + Sync + 'static>(mut self, ext: T) -> Self {
        self.ext = Some(Box::new(ext));
        self
    }
    pub fn assert<T: Any>(&self) -> bool {
        self.ext
            .as_ref()
            .map(|args| args.downcast_ref::<T>().is_some())
            .unwrap_or(false)
    }
    pub fn into_inner<T: Any + Send + Sync + 'static>(&mut self) -> Option<T> {
        if self.assert::<T>() {
            let t = self.ext.take().unwrap().downcast::<T>().unwrap();
            Some(*t)
        } else {
            None
        }
    }
    pub fn deref_mut_args<T: Any, Out>(
        &mut self,
        handle: impl FnOnce(Option<&mut T>) -> Out,
    ) -> Out {
        let opt = self
            .ext
            .as_mut()
            .and_then(|args| args.downcast_mut::<T>());
        handle(opt)
    }
    pub fn deref_args<T: Any, Out>(&self, handle: impl FnOnce(Option<&T>) -> Out) -> Out {
        let opt = self
            .ext
            .as_ref()
            .and_then(|args| args.downcast_ref::<T>());
        handle(opt)
    }
}

#[derive(Debug)]
pub struct TaskResult {
    pub code: i32,
    pub msg: String,
    // 任务id
    pub task_id: String,
    // 任务所属agent
    pub agent_id: String,
    // 任务结果数据
    pub data: Option<Box<dyn Any + Send + 'static>>,
    // task的extend信息
    pub extend: Option<HashMap<String, String>>,
}

impl TaskResult {
    pub fn new<M: Into<String>, T: Into<String>, A: Into<String>>(
        code: i32,
        msg: M,
        task_id: T,
        agent_id: A,
    ) -> Self {
        Self {
            code,
            msg: msg.into(),
            data: None,
            task_id: task_id.into(),
            agent_id: agent_id.into(),
            extend: None,
        }
    }
    pub fn success<T: Into<String>, A: Into<String>>(task_id: T, agent_id: A) -> Self {
        Self::new(0, "success", task_id, agent_id)
    }
    pub fn error<M: Into<String>, T: Into<String>, A: Into<String>>(
        code: i32,
        msg: M,
        task_id: T,
        agent_id: A,
    ) -> Self {
        Self::new(code, msg, task_id, agent_id)
    }
    pub fn is_error(&self) -> bool {
        self.code != 0
    }
    pub fn is_success(&self) -> bool {
        self.code == 0
    }
    pub fn set_extend(mut self, extend: HashMap<String, String>) -> Self {
        self.extend = Some(extend);
        self
    }
    pub fn set_data<T: Any + Send + 'static>(mut self, data: T) -> Self {
        self.data = Some(Box::new(data));
        self
    }
    pub fn set_data_raw(mut self, data: Box<dyn Any + Send + 'static>) -> Self {
        self.data = Some(data);
        self
    }
    pub fn assert<T: Any>(&self) -> bool {
        if let Some(ref data) = self.data {
            data.downcast_ref::<T>().is_some()
        } else {
            false
        }
    }
    pub fn into_inner<T: Any>(&mut self) -> Option<T> {
        if let Some(data) = self.data.take() {
            match data.downcast::<T>() {
                Ok(t) => Some(*t),
                Err(e) => {
                    self.data = Some(e);
                    None
                }
            }
        } else {
            None
        }
    }
}

impl Display for TaskResult {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[TaskResult] code: {}, msg: {}, task_id: {}, agent_id: {}, data: {:?}",
            self.code, self.msg, self.task_id, self.agent_id, self.data
        )
    }
}
