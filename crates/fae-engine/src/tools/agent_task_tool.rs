use chrono::Timelike;
use serde_json::Value;
use wd_tools::channel::Channel;
use fae_agent::{AgentInfo, AgentTask, AgentTaskStatus, ToolResponse, GLOBAL_KEY_AGENT_ID, GLOBAL_KEY_SESSION_ID};
use crate::{IdenInfo, Tool};

pub const AGENT_TASK_TOOL_NAME: &str = "agent_exec_task";

#[derive(Debug)]
pub struct AgentTaskTool{
    chan:Channel<AgentTask>
}

impl AgentTaskTool {
    pub fn new(chan:Channel<AgentTask>) -> Self {
        Self{
            chan,
        }
    }
}

#[async_trait::async_trait]
impl Tool for AgentTaskTool {
    fn name(&self) -> &str {
        AGENT_TASK_TOOL_NAME
    }

    fn description(&self) -> &str {
        // 支持发布任务，执行任务，执行完成，执行失败，四种更新方式。
        "Manage agent task lifecycle. Supports publishing a task to agent, marking it as executing, completed, or failed."
    }

    fn arguments(&self) -> Value {
        AgentTask::arguments()
    }

    async fn call(&self, iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let mut task: AgentTask = serde_json::from_str(&args)
            .map_err(|e| anyhow::anyhow!("[AgentTaskTool] invalid arguments: {}", e))?;
        match &mut task.content {
            AgentTaskStatus::Create(create) => {
                if create.to_agent.agent_id.is_empty() {
                    return Err(anyhow::anyhow!("[AgentTaskTool] to_agent.agent_id is empty"));
                }
                create.from_agent = AgentInfo{
                    agent_id: iden.get(GLOBAL_KEY_AGENT_ID).unwrap_or(iden.get_agent_id().to_string()),
                    session_id: iden.get(GLOBAL_KEY_SESSION_ID).unwrap_or("".to_string()),
                    user_id: iden.get_user_id().to_string(),
                };
                task.task_id = iden.task_id;
            }
            AgentTaskStatus::Executing(executing) => {
                if task.task_id.is_empty() {
                    return Err(anyhow::anyhow!("[AgentTaskTool] task_id is empty"));
                }
                executing.timestamp = wd_tools::time::Utc::now().second() as u64;
            }
            AgentTaskStatus::Completed(_result) => {
                if task.task_id.is_empty() {
                    return Err(anyhow::anyhow!("[AgentTaskTool] task_id is empty"));
                }
            }
            AgentTaskStatus::Failed(_result) => {
                if task.task_id.is_empty() {
                    return Err(anyhow::anyhow!("[AgentTaskTool] task_id is empty"));
                }
            }
        }
        self.chan.send(task).await?;
        Ok(ToolResponse::with_result("Task update successfully.".into()))
    }
}