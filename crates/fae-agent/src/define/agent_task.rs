use serde::{Deserialize, Serialize};
use std::any::Any;

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
    #[serde(default)]
    pub from_agent: AgentInfo,
    pub to_agent: AgentInfo,
    pub content: String,
}
#[derive(Default, Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[serde(default)]
pub struct AgentTaskExecuting {
    // 时间戳, utc second
    pub timestamp: u64,
    pub content: String,
}

#[derive(Default, Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[serde(default)]
pub struct AgentTaskResult {
    #[serde(default)]
    pub task_author: AgentInfo,
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
#[derive(Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct AgentTask {
    pub task_id: String,
    pub content: AgentTaskStatus,
}

impl AgentTask {
    pub fn get_push_agent_id(&self) -> &str {
        match &self.content {
            AgentTaskStatus::Create(create) => &create.to_agent.agent_id,
            AgentTaskStatus::Executing(exec) => "",
            AgentTaskStatus::Completed(result) => &result.task_author.agent_id,
            AgentTaskStatus::Failed(result) => &result.task_author.agent_id,
        }
    }
    pub fn arguments() -> serde_json::Value {
        let agent_info = serde_json::json!({
            "type": "object",
            "properties": {
                "agent_id": {"type": "string"},
                "session_id": {"type": "string"},
                "user_id": {"type": "string"}
            }
        });
        let task_result = serde_json::json!({
            "type": "object",
            "properties": {
                "content": {"type": "string"}
            },
            "required": ["content"]
        });

        serde_json::json!({
            "type": "object",
            "description": "Update a task status. The shape must match AgentTask: task_id plus content containing exactly one lifecycle status.",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task id."
                },
                "content": {
                    "type": "object",
                    "description": "The task lifecycle status. Exactly one status field must be provided.",
                    "properties": {
                        "create": {
                            "type": "object",
                            "description": "Publish a new task.",
                            "properties": {
                                "to_agent": agent_info,
                                "content": {"type": "string", "description": "The task content."}
                            },
                            "required": ["to_agent", "content"]
                        },
                        "executing": {
                            "type": "object",
                            "description": "Mark the task as executing.",
                            "properties": {
                                "content": {"type": "string", "description": "The task is running."}
                            }
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
                }
            },
            "required": ["task_id", "content"]
        })
    }
}

#[derive(Debug)]
pub struct AgentTaskExt {
    pub task: AgentTask,
    pub ext: Option<Box<dyn Any + Sync + Send + 'static>>,
}
impl AgentTaskExt {
    pub fn new(task: AgentTask) -> Self {
        Self { task, ext: None }
    }
    pub fn set(self, ext: Box<dyn Any + Sync + Send + 'static>) -> Self {
        Self {
            task: self.task,
            ext: Some(ext),
        }
    }
    pub fn try_ext_into<T: Any>(&mut self) -> Option<T> {
        if let Some(ext) = self.ext.as_deref() {
            if ext.downcast_ref::<T>().is_none() {
                return None;
            }
        } else {
            return None;
        }
        let t = self.ext.take().unwrap();
        let x = t.downcast::<T>().unwrap();
        Some(*x)
    }
}
