use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// 任务类型，表示智能体需要执行的任务
#[derive(Debug,Serialize, Deserialize)]
pub struct Task{
    pub id: String,
    pub agent_id: String,
    pub r#type: TaskType,
    pub args: Value,
}
impl Task {
    pub fn new(id: String, agent_id: String, r#type: TaskType, args: Value) -> Self {
        Self { id, agent_id, r#type, args }
    }
    pub fn set_id<T:Into<String>>(mut self, id: T)->Self {
        self.id = id.into();
        self
    }
    pub fn set_type<T:Into<TaskType>>(mut self, t: T) -> Self {
        self.r#type = t.into();
        self
    }
    pub fn set_args<T:Into<Value>>(mut self, args: T) -> Self {
        self.args = args.into();
        self
    }
    pub fn set_args_json<T:Serialize>(mut self, args: T) -> Result<Self,serde_json::Error>{
        self.args = serde_json::to_value(&args)?;
        Ok(self)
    }
    pub fn set_agent_id<T:Into<String>>(mut self, agent_id: T)->Self {
        self.agent_id = agent_id.into();
        self
    }
}
impl Default for Task {
    fn default() -> Self {
        Self::new("".to_string(), "".to_string(),  TaskType::None, Value::Null)
    }
}

/// 任务类型，表示智能体需要执行的任务
#[derive(Default,Debug, PartialEq,Eq,Clone,PartialOrd,Ord,Hash,Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskType {
    /// 无任务
    #[default]
    None,
    /// 执行模块
    Module,
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
            "module" => TaskType::Module,
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

#[derive(Default, Debug)]
pub struct  TaskResult{
    // 0:成功，其他:失败
    pub code: i32,
    pub msg : String,
    pub data: Value,
    // 任务id
    pub task_id: String,
    // 任务所属agent
    pub agent_id: String,
}

impl TaskResult {
    pub fn new<M:Into<String>,T:Into<String>,A:Into<String>>(code:i32,msg:M,task_id: T,agent_id:A)->Self{
        Self {
            code,
            msg: msg.into(),
            data: Value::Null,
            task_id: task_id.into(),
            agent_id: agent_id.into(),
        }
    }
    pub fn set_raw_data<T:Into<Value>>(mut self, data:T)->Self{
        self.data = data.into();
        self
    }
    pub fn must_set_json_data<T:Serialize>(mut self, data:T)->Self{
        self.data = serde_json::to_value(&data).unwrap();
        self
    }
}

#[async_trait::async_trait]
pub trait TaskExecutor:Sync{
    fn desc(&self) -> String;
    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_task_type_deserialize() {
        let mut task = Task::default().set_id("123").set_type("tool").set_args_json(r#"{"input":"hello world"}"#).unwrap();
        println!("{:?}",serde_json::to_string(&task).unwrap());
        task = task.set_type("custom");
        println!("{:?}",serde_json::to_string(&task).unwrap());
        let t1= serde_json::from_str::<Task>(r#"{"id":"123","type":"custom","args":{"input":"hello world"}}"#).unwrap();
        println!("{:?}",t1);
        assert_eq!(task.r#type, t1.r#type);
    }
}