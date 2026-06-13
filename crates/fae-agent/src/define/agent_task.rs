use serde::{Deserialize, Serialize};
use std::any::Any;
use std::time;

#[derive(Default, Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[serde(default)]
pub struct AgentInfo {
    pub agent_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub user_id: String,
}

#[derive(Default, Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct AgentTaskCreate {
    pub executor: AgentInfo,
    pub content: String,
}
#[derive(Default, Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[serde(default)]
pub struct AgentTaskExecuting {
    pub task_id: String,
}

#[derive(Default, Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[serde(default)]
pub struct AgentTaskResult {
    pub task_id: String,
    pub content: String,
}

#[derive(Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentTaskStatus {
    Create(AgentTaskCreate),
    //执行中
    Executing(AgentTaskExecuting),
    //执行完成
    Completed(AgentTaskResult),
    //执行完成
    Failed(AgentTaskResult),
}
impl AgentTaskStatus {
    pub const CREATE: &'static str = "create";
    pub const EXECUTING: &'static str = "executing";
    pub const COMPLETED: &'static str = "completed";
    pub const FAILED: &'static str = "failed";
    pub fn get_task_id(&self) -> &str {
        match self {
            AgentTaskStatus::Create(_create) => "",
            AgentTaskStatus::Executing(exec) => &exec.task_id,
            AgentTaskStatus::Completed(result) => &result.task_id,
            AgentTaskStatus::Failed(result) => &result.task_id,
        }
    }
    pub fn status(&self) -> &str {
        match self {
            AgentTaskStatus::Create(_create) => Self::CREATE,
            AgentTaskStatus::Executing(_exec) => Self::EXECUTING,
            AgentTaskStatus::Completed(_result) => Self::COMPLETED,
            AgentTaskStatus::Failed(_result) => Self::FAILED,
        }
    }
}

impl AgentTaskStatus {
    pub fn arguments() -> serde_json::Value {
        let agent_info = serde_json::json!({
            "type": "object",
            "properties": {
                "agent_id": {"type": "string"},
                "session_id": {"type": "string"},
                "user_id": {"type": "string"}
            },
            "required": ["agent_id"]
        });
        let task_result = serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task id."
                },
                "content": {"type": "string"}
            },
            "required": ["task_id", "content"]
        });

        serde_json::json!({
            "type": "object",
            "description": "Update an agent task lifecycle status. The shape must match AgentTaskStatus and contain exactly one lifecycle status.",
            "properties": {
                "create": {
                    "type": "object",
                    "description": "Publish a new task. task_id is auto generated.",
                    "properties": {
                        "executor": agent_info,
                        "content": {"type": "string", "description": "The task content."}
                    },
                    "required": ["executor", "content"]
                },
                "executing": {
                    "type": "object",
                    "description": "Mark the task as executing. task_id must be provided.",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "The task id."
                        }
                    },
                    "required": ["task_id"]
                },
                "completed": task_result.clone(),
                "failed": task_result
            },
            "oneOf": [
                {"required": ["create"]},
                {"required": ["executing"]},
                {"required": ["completed"]},
                {"required": ["failed"]}
            ]
        })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct AgentTask {
    pub task_id: String,
    // 任务发布者
    pub author: AgentInfo,
    // 任务执行者
    pub executor: AgentInfo,
    // 任务状态
    pub status: String,
    // 任务内容
    pub content: String,
    // 任务结果
    pub result: String,
    // 更新时间,utc second
    pub update_timestamp: u64,
    #[serde(skip)]
    pub extend: Option<Box<dyn Any + Sync + Send + 'static>>,
}

impl AgentTask {
    pub fn set_task_id(mut self, task_id: String) -> Self {
        self.task_id = task_id;
        self
    }
    pub fn get_task_id(&self) -> &str {
        self.task_id.as_str()
    }
    pub fn set_executor(mut self, executor: AgentInfo) -> Self {
        self.executor = executor;
        self
    }
    pub fn set_author(mut self, author: AgentInfo) -> Self {
        self.author = author;
        self
    }
    pub fn get_author_id(&self) -> &str {
        &self.author.agent_id
    }
    pub fn get_executor_id(&self) -> &str {
        &self.executor.agent_id
    }
    pub fn get_agent_id(&self) -> &str {
        if self.status == AgentTaskStatus::CREATE {
            &self.executor.agent_id
        } else {
            &self.author.agent_id
        }
    }
    pub fn set_status(mut self, status: String) -> Self {
        self.status = status;
        self
    }
    pub fn get_status(&self) -> &str {
        self.status.as_str()
    }
    pub fn set_content(mut self, content: String) -> Self {
        self.content = content;
        self
    }
    pub fn set_result(mut self, result: String) -> Self {
        self.result = result;
        self
    }
    pub fn update_timestamp(&mut self) {
        self.update_timestamp = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as u64;
    }
    pub fn set_ext(&mut self, ext: Box<dyn Any + Sync + Send + 'static>) {
        self.extend = Some(ext);
    }
    pub fn try_ext_into<T: Any>(&mut self) -> Option<T> {
        if let Some(ext) = self.extend.as_deref() {
            if ext.downcast_ref::<T>().is_none() {
                return None;
            }
        } else {
            return None;
        }
        let t = self.extend.take().unwrap();
        let x = t.downcast::<T>().unwrap();
        Some(*x)
    }
}

impl Clone for AgentTask {
    fn clone(&self) -> Self {
        Self {
            task_id: self.task_id.clone(),
            author: self.author.clone(),
            executor: self.executor.clone(),
            status: self.status.clone(),
            content: self.content.clone(),
            result: self.result.clone(),
            update_timestamp: self.update_timestamp,
            extend: None,
        }
    }
}
