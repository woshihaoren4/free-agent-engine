use crate::{IdenInfo, Tool};
use async_trait::async_trait;
use fae_agent::{
    Env, EnvEvent, Environment, Select, TaskResult, TaskType, Thing, ThingItem, ThingSelect,
    ToolRequest, ToolResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use wd_tools::PFErr;

pub const TASK_RUNTIME_ID: &str = "FAE_TASK_RUNTIME";
pub const TASK_TOOL_NAME: &str = "task";

#[derive(Default, Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct AgentInfo {
    pub agent_id: String,
    pub session_id: String,
    pub user_id: String,
}

#[derive(Default, Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct TaskCreate {
    pub from_agent: AgentInfo,
    pub to_agent: AgentInfo,
    pub content: String,
}
#[derive(Default, Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct TaskExecuting {
    pub timestamp: u64,
}
#[derive(Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Create(TaskCreate),
    //执行中
    Executing(TaskExecuting),
    //执行完成
    Completed(String),
    //执行完成
    Failed(String),
}
#[derive(Debug, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Task {
    pub task_id: String,
    pub content: TaskStatus,
}

#[derive(Debug)]
pub struct TaskTool {
    pub tasks: Arc<RwLock<HashMap<String, Task>>>,
}

impl TaskTool {
    pub fn new(tasks: Arc<RwLock<HashMap<String, Task>>>) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        TASK_TOOL_NAME
    }

    fn description(&self) -> &str {
        // 支持发布任务，执行任务，执行完成，执行失败，四种更新方式。
        "Manage task lifecycle. Supports publishing a task, marking it as executing, completed, or failed."
    }

    fn arguments(&self) -> Value {
        //对应TaskStatus
        serde_json::json!({
            "type": "object",
            "description": "Update a task status. Exactly one of the following fields must be provided, corresponding to the task lifecycle.",
            "properties": {
                "create": {
                    "type": "object",
                    "description": "Publish a new task.",
                    "properties": {
                        "from_agent": {
                            "type": "object",
                            "description": "The agent that creates the task.",
                            "properties": {
                                "agent_id": {"type": "string"},
                                "session_id": {"type": "string"},
                                "user_id": {"type": "string"}
                            },
                            "required": ["agent_id", "session_id", "user_id"]
                        },
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
                    "required": ["from_agent", "to_agent", "content"]
                },
                "executing": {
                    "type": "object",
                    "description": "Mark the task as executing.",
                    "properties": {
                        "timestamp": {"type": "integer", "description": "The timestamp when the task starts executing."}
                    },
                    "required": ["timestamp"]
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

    async fn call(&self, iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let task_id = iden.task_id;
        let content: TaskStatus = serde_json::from_str(&args)
            .map_err(|e| anyhow::anyhow!("[TaskTool] invalid arguments: {}", e))?;
        let task = Task {
            task_id: task_id.clone(),
            content,
        };
        // 插入 tasks 中
        let mut tasks = self.tasks.write().await;
        tasks.insert(task_id.clone(), task);
        Ok(ToolResponse::with_result(format!(
            "Task[{}] updated successfully.",
            task_id
        )))
    }
}

#[derive(Debug)]
pub struct TaskRuntime {
    pub tasks: Arc<RwLock<HashMap<String, Task>>>,
    parent: Option<Env>,
}

impl TaskRuntime {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            parent: None,
        }
    }
    pub fn get_tool(&self) -> TaskTool {
        TaskTool::new(self.tasks.clone())
    }
    pub async fn exec_tool(&self, mut task: fae_agent::Task) -> anyhow::Result<TaskResult> {
        if let Some(req) = task.into_inner::<ToolRequest>() {
            let tool = self.get_tool();
            let iden = IdenInfo::new(
                task.id.clone(),
                task.agent_id.clone(),
                task.user_id.clone(),
                task.ctx.clone(),
            );
            match tool.call(iden, req.arguments).await {
                Ok(resp) => Ok(TaskResult::success(task.id, task.agent_id).set_data(resp)),
                Err(e) => {
                    let info = format!("Tool[{}] call failed. error: {}", req.tool_name, e);
                    Ok(TaskResult::success(task.id, task.agent_id)
                        .set_data(ToolResponse::with_result(info)))
                }
            }
        } else {
            anyhow::anyhow!("[TaskRuntime] task is not a tool request").err()
        }
    }
    pub fn is_task_tool_task(task: &fae_agent::Task) -> bool {
        task.get_type() == &TaskType::Tool
            && task.get_exec_channel() == "default"
            && task.deref_args::<ToolRequest, bool>(|x| {
                if let Some(r) = x {
                    r.tool_name.as_str() == TASK_TOOL_NAME
                } else {
                    false
                }
            })
    }
}

#[async_trait::async_trait]
impl Environment for TaskRuntime {
    fn id(&self) -> &'static str {
        TASK_RUNTIME_ID
    }

    async fn register_parent_env(&mut self, env: Env) {
        self.parent = Some(env);
    }

    async fn watch(&self) -> anyhow::Result<EnvEvent> {
        if let Some(ref p) = self.parent {
            p.watch().await
        } else {
            std::future::pending().await
        }
    }

    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>> {
        if let ThingSelect::Tool(chan, name) = &select.select {
            if name == TASK_TOOL_NAME && chan == "default" {
                let tool = self.get_tool();
                return Ok(vec![
                    Thing::new(self.id().to_string())
                        .add_item(ThingItem::Tool(
                            tool.description().to_string(),
                            tool.arguments(),
                        ))
                        .into_self(),
                ]);
            }
        }
        if let Some(ref p) = self.parent {
            p.query(select).await
        } else {
            Ok(Vec::new())
        }
    }

    async fn spawn(&self, tasks: Vec<fae_agent::Task>) -> anyhow::Result<()> {
        let (ts, ptasks): (Vec<_>, Vec<_>) =
            tasks.into_iter().partition(|t| Self::is_task_tool_task(t));
        if !ptasks.is_empty() {
            if let Some(ref p) = self.parent {
                p.spawn(ptasks).await?;
            } else {
                return anyhow::anyhow!("[TaskRuntime] spawn failed, no parent").err();
            }
        }
        for task in ts {
            self.exec_tool(task).await?;
        }
        Ok(())
    }

    async fn execute(&self, task: fae_agent::Task) -> anyhow::Result<TaskResult> {
        if Self::is_task_tool_task(&task) {
            return self.exec_tool(task).await;
        }
        if let Some(ref p) = self.parent {
            p.execute(task).await
        } else {
            anyhow::anyhow!("[TaskRuntime] execute failed, no parent").err()
        }
    }
}
