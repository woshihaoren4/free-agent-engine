use std::str::FromStr;
use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use cron::Schedule;
use fae_agent::{ToolResponse, GLOBAL_KEY_AGENT_ID, GLOBAL_KEY_SESSION_ID};
use serde_json::Value;
use wd_tools::channel::Channel;
use wd_tools::PFErr;

pub const SCHEDULED_EXECUTION_TOOL_NAME: &str = "scheduled_execution";

#[derive(Debug, Clone)]
pub struct ScheduledTask {
    pub cron_expression: String,
    pub execute_once: bool,
    pub task_content: String,
    pub agent_id: String,
    pub plan_id: String,
    pub session_id: String,
    pub user_id: String,
}

#[derive(Debug)]
pub struct ScheduledExecution {
    channel: Channel<ScheduledTask>,
}

impl ScheduledExecution {
    pub fn new(channel: Channel<ScheduledTask>) -> Self {
        Self { channel }
    }
}

#[async_trait]
impl Tool for ScheduledExecution {
    fn name(&self) -> &str {
        SCHEDULED_EXECUTION_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Submit a scheduled task with a cron expression. Allows executing a task once or periodically."
    }

    fn arguments(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "cron_expression": {
                    "type": "string",
                    "description": "The cron expression for the scheduled task (e.g. '0 0/1 * * * *' for every minute, '0/10 * * * * *' for every 10 seconds)."
                },
                "execute_once": {
                    "type": "boolean",
                    "description": "If true, the task will only be executed once when the time arrives. If false, it will be executed periodically."
                },
                "task_content": {
                    "type": "string",
                    "description": "The content of the task to be executed. Must be clear enough for the agent to perform."
                }
            },
            "required": ["cron_expression", "execute_once", "task_content"]
        })
    }

    async fn call(&self, iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let cron_expression = args_val["cron_expression"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("cron_expression is required"))?
            .to_string();
        //校验表达式是否合法
        if let Err(e) = Schedule::from_str(&cron_expression){
            return anyhow::anyhow!("Invalid cron expression: {}", e).err()
        }
        let execute_once = args_val["execute_once"]
            .as_bool()
            .unwrap_or(true);
        let task_content = args_val["task_content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("task_content is required"))?
            .to_string();
        let session_id = iden.get(GLOBAL_KEY_SESSION_ID).ok_or_else(|| anyhow::anyhow!("session_id is required"))?.to_string();
        let agent_id = iden.get(GLOBAL_KEY_AGENT_ID).unwrap_or(iden.get_agent_id().to_string());


        let task = ScheduledTask {
            cron_expression,
            execute_once,
            task_content,
            agent_id,
            plan_id: iden.get_task_id().to_string(),
            session_id,
            user_id: iden.get_user_id().to_string(),
        };
        self.channel.send(task).await.map_err(|e| anyhow::anyhow!("Failed to submit scheduled task: {}", e))?;

        Ok(ToolResponse::with_result("Scheduled task submitted successfully.".to_string()))
    }
}
