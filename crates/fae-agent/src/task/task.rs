use serde::{Deserialize, Deserializer, Serialize};
use std::any::Any;
use std::error::Error;
use std::fmt::{Display, Formatter};

/// 任务类型，表示智能体需要执行的任务
#[derive(Default, Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize)]
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
            "model" | "module" => TaskType::Model,
            "plan" => TaskType::Plan,
            "tools" => TaskType::Tool,
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
        let s = match self {
            TaskType::None => "none",
            TaskType::Model => "model",
            TaskType::Plan => "plan",
            TaskType::Tool => "tools",
            TaskType::Agent => "agent",
            TaskType::Skill => "skill",
            TaskType::Custom => "custom",
            TaskType::Output => "output",
            TaskType::Error => "error",
            TaskType::Over => "over",
            TaskType::Any(s) => s.as_str(),
        };
        write!(f, "{}", s)
    }
}

/// 任务类型，表示智能体需要执行的任务
#[derive(Debug)]
pub struct Task {
    pub id: String,
    pub agent_id: String,
    pub r#type: TaskType,
    pub exec_channel: String,
    pub user_id: String,
    pub args: Option<Box<dyn Any + Send + Sync + 'static>>,
}
impl Task {
    pub fn new<I: Into<String>, A: Into<String>>(id: I, agent_id: A, r#type: TaskType) -> Self {
        let id = id.into();
        let agent_id = agent_id.into();
        Self {
            id,
            agent_id,
            r#type,
            args: None,
            user_id: "".into(),
            exec_channel: "default".into(),
        }
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
    pub fn set_type<T: Into<TaskType>>(mut self, t: T) -> Self {
        self.r#type = t.into();
        self
    }
    pub fn get_type(&self) -> &TaskType {
        &self.r#type
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
        self.exec_channel = channel.into();self
    }
    pub fn get_channel(&self) -> &str {
        self.exec_channel.as_str()
    }
    pub fn set_args_raw(mut self, args: Box<dyn Any + Send + Sync + 'static>) -> Self {
        self.args = Some(args);
        self
    }
    pub fn set_args<T: Any + Send + Sync + 'static>(self, args: T) -> Self {
        self.set_args_raw(Box::new(args))
    }
    pub fn assert<T: Any>(&self) -> bool {
        if let Some(ref args) = self.args {
            args.downcast_ref::<T>().is_some()
        } else {
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
                }
            }
        } else {
            None
        }
    }
}

impl Default for Task {
    fn default() -> Self {
        let id = wd_tools::uuid::v4();
        let aid = "".to_string();
        Self::new(id, aid, TaskType::None)
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
    pub fn set_raw_data<T: Any + Send + 'static>(mut self, data: T) -> Self {
        self.data = Some(Box::new(data));
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
