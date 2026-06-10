use serde::{Deserialize, Serialize};

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
    pub fn arguments()-> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "description": "Update a task status. Exactly one of the following fields must be provided, corresponding to the task lifecycle.",
            "properties": {
                "create": {
                    "type": "object",
                    "description": "Publish a new task.",
                    "properties": {
                        "to_agent": {
                            "type": "object",
                            "description": "The agent that will execute the task.",
                            "properties": {
                                "agent_id": {"type": "string"},
                                "session_id": {"type": "string"},
                                "user_id": {"type": "string"}
                            },
                            "required": ["agent_id", "session_id", "user_id"]
                        },
                        "content": {"type": "string", "description": "The task content."}
                    },
                    "required": ["to_agent", "content"]
                },
                "executing": {
                    "type": "object",
                    "description": "Mark the task as executing.",
                    "properties": {
                        "content": {"type": "string", "description": "The task is running."}
                    },
                    "required": []
                },
                "completed": {
                    "type": "string",
                    "description": "Mark the task as completed with the result."
                },
                "failed": {
                    "type": "string",
                    "description": "Mark the task as failed with the reason."
                }
            }
        })
    }
}