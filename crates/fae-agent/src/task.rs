use std::any::Any;
use std::fmt::Display;
use serde::{Deserialize, Deserializer, Serialize};

/// 任务类型，表示智能体需要执行的任务
#[derive(Debug)]
pub struct Task{
    pub id: String,
    pub agent_id: String,
    pub r#type: TaskType,
    pub exec_channel: String,
    pub args: Option<Box<dyn Any + Send + Sync + 'static>>,
}
impl Task {
    pub fn new<I:Into<String>,A:Into<String>>(id: I, agent_id: A, r#type: TaskType) -> Self {
        let id = id.into();
        let agent_id = agent_id.into();
        Self { id, agent_id, r#type, args: None,exec_channel:"default".into() }
    }
    pub fn set_id<T:Into<String>>(mut self, id: T)->Self {
        self.id = id.into();
        self
    }
    pub fn get_id(&self) -> &str { self.id.as_str() }
    pub fn get_agent_id(&self) -> &str { self.agent_id.as_str() }
    pub fn get_exec_channel(&self) -> &str { self.exec_channel.as_str() }
    pub fn set_type<T:Into<TaskType>>(mut self, t: T) -> Self {
        self.r#type = t.into();
        self
    }
    pub fn get_type(&self) -> &TaskType { &self.r#type }
    pub fn set_exec_channel<T:Into<String>>(mut self, t: T) -> Self {
        self.exec_channel = t.into();
        self
    }
    pub fn set_agent_id<T:Into<String>>(mut self, agent_id: T)->Self {
        self.agent_id = agent_id.into();
        self
    }
    pub fn set_args<T:Any + Send + Sync + 'static>(mut self, args: T) -> Self {
        self.args = Some(Box::new(args));
        self
    }
    pub fn assert<T: Any>(&self) -> bool {
        if let Some(ref args) = self.args {
            args.downcast_ref::<T>().is_some()
        }else{
            false
        }
    }
    pub fn into_inner<T: Any>(&mut self) -> Option<T> {
        if let Some(args) = self.args.take() {
            match args.downcast::<T>() {
                Ok(t) => Some(*t),
                Err(e) => {
                    self.args = Some(e);
                    None
                },
            }
        }else{
            None
        }
    }
}

/// 任务类型，表示智能体需要执行的任务
#[derive(Default,Debug, PartialEq,Eq,Clone,PartialOrd,Ord,Hash,Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskType {
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
impl<'de> Deserialize<'de> for TaskType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(TaskType::from(s.as_str()))
    }
}
impl From<&str> for TaskType {
    fn from(s: &str) -> Self {
        match s {
            "none" => TaskType::None,
            "module" => TaskType::Model,
            "tool" => TaskType::Tool,
            "agent" => TaskType::Agent,
            "skill" => TaskType::Skill,
            "custom" => TaskType::Custom,
            "output" => TaskType::Output,
            "error" => TaskType::Error,
            "over" => TaskType::Over,
            other => TaskType::Any(other.to_string()),
        }
    }
}
impl Display for TaskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string())
    }
}
#[derive(Debug)]
pub struct  TaskResult{
    pub code: i32,
    pub msg: String,
    // 任务id
    pub task_id: String,
    // 任务所属agent
    pub agent_id: String,
    // 任务结果数据
    pub data: Option<Box<dyn Any + Send + 'static>>,
}

impl TaskResult {
    pub fn new<M:Into<String>,T:Into<String>,A:Into<String>>(code:i32,msg:M,task_id: T,agent_id:A)->Self{
        Self {
            code,
            msg:msg.into(),
            data: None,
            task_id: task_id.into(),
            agent_id: agent_id.into(),
        }
    }
    pub fn success<T:Into<String>,A:Into<String>>(task_id: T,agent_id:A)->Self{
        Self::new(0,"success",task_id,agent_id)
    }
    pub fn is_error(&self) -> bool {
        self.code != 0
    }
    pub fn set_raw_data<T:Any + Send + 'static>(mut self, data:T)->Self{
        self.data = Some(Box::new(data));
        self
    }
    pub fn assert<T:Any>(&self) -> bool {
        if let Some(ref data) = self.data {
            data.downcast_ref::<T>().is_some()
        }else{
            false
        }
    }
    pub fn into_inner<T:Any>(&mut self) -> Option<T>{
        if let Some(data) = self.data.take() {
            match data.downcast::<T>() {
                Ok(t) => Some(*t),
                Err(e) => {
                    self.data = Some(e);
                    None
                },
            }
        }else{
            None
        }
    }
}

#[async_trait::async_trait]
pub trait TaskExecutor:Sync{
    fn desc(&self) -> String;
    fn channel(&self) -> String{
        "default".to_string()
    }
    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult>;
}
