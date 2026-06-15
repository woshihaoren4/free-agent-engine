use crate::agent_runtime::AgentTaskStore;
use crate::{IdenInfo, Tool};
use chrono::Timelike;
use fae_agent::{
    AgentInfo, AgentTask, AgentTaskStatus, GLOBAL_KEY_AGENT_ID, GLOBAL_KEY_SESSION_ID, ToolResponse,
};
use serde_json::Value;
use std::sync::Arc;
use wd_tools::channel::Channel;

pub const AGENT_TASK_TOOL_NAME: &str = "agent_exec_task";

#[derive(Debug)]
pub struct AgentTaskTool {
    chan: Channel<AgentTask>,
    pub task_store: Arc<dyn AgentTaskStore + Send + Sync + 'static>,
}

impl AgentTaskTool {
    pub fn new(
        chan: Channel<AgentTask>,
        task_store: Arc<dyn AgentTaskStore + Send + Sync + 'static>,
    ) -> Self {
        Self { chan, task_store }
    }
}

#[async_trait::async_trait]
impl Tool for AgentTaskTool {
    fn name(&self) -> &str {
        AGENT_TASK_TOOL_NAME
    }

    fn description(&self) -> &str {
        // 支持发布任务，执行任务，执行完成，执行失败，四种更新方式。
        "Manage agent task lifecycle. Supports publishing a task to agent, marking it as executing, completed, or failed. Note: The task content and results must be very detailed."
    }

    fn arguments(&self) -> Value {
        AgentTaskStatus::arguments()
    }

    async fn call(&self, iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let agent_id = iden
            .get(GLOBAL_KEY_AGENT_ID)
            .unwrap_or(iden.get_agent_id().to_string());
        let session_id = iden.get(GLOBAL_KEY_SESSION_ID).unwrap_or("".to_string());
        let channel = self.chan.clone();
        let mut task: AgentTaskStatus = serde_json::from_str(&args)
            .map_err(|e| anyhow::anyhow!("[AgentTaskTool] invalid arguments: {}", e))?;
        let task_id = task.get_task_id().to_string();
        let status = task.status().to_string();
        match task {
            AgentTaskStatus::Create(mut create) => {
                //发布者
                let author = AgentInfo {
                    agent_id: iden
                        .get(GLOBAL_KEY_AGENT_ID)
                        .unwrap_or(iden.get_agent_id().to_string()),
                    session_id: iden.get(GLOBAL_KEY_SESSION_ID).unwrap_or("".to_string()),
                    user_id: iden.get_user_id().to_string(),
                };
                //记录任务
                if create.executor.session_id.is_empty() {
                    create.executor.session_id = session_id.to_string();
                }
                if create.executor.user_id.is_empty() {
                    create.executor.user_id = author.user_id;
                }
                let mut task = AgentTask::default()
                    .set_task_id(wd_tools::uuid::v4())
                    .set_status(status)
                    .set_author(author)
                    .set_executor(create.executor)
                    .set_content(create.content);
                task.update_timestamp();
                self.task_store.push_task(task.clone()).await;
                //挂钩子，等当前agent执行完成，则开始执行任务
                fae_agent::Hook::agent_call_session_over(
                    &agent_id,
                    &session_id,
                    |_ctx, output| async move {
                        task.set_ext(output);
                        channel.send(task).await?;
                        Ok(())
                    },
                );
            }
            AgentTaskStatus::Executing(executing) => {
                if executing.task_id.is_empty() {
                    return Err(anyhow::anyhow!(
                        "[AgentTaskTool] executing.task_id is empty"
                    ));
                }
                self.task_store
                    .update_status_executing(executing.task_id.as_str())
                    .await?;
                //不需要挂钩子，因为当前agent正在执行任务，等当前任务完成，再执行下一个任务
            }
            AgentTaskStatus::Completed(result) => {
                if result.task_id.is_empty() {
                    return Err(anyhow::anyhow!(
                        "[AgentTaskTool] completed.task_id is empty"
                    ));
                }
                //移除任务
                let task = self.task_store.remove_task(result.task_id.as_str()).await;
                let mut task = if let Some(mut t) = task {
                    t.set_result(result.content).set_status(status)
                } else {
                    return Err(anyhow::anyhow!(
                        "[AgentTaskTool] completed task[{}] buf not found .",
                        result.task_id
                    ));
                };
                fae_agent::Hook::agent_call_session_over(
                    &agent_id,
                    &session_id,
                    |_ctx, output| async move {
                        task.set_ext(output);
                        channel.send(task).await?;
                        Ok(())
                    },
                );
            }
            AgentTaskStatus::Failed(result) => {
                if result.task_id.is_empty() {
                    return Err(anyhow::anyhow!("[AgentTaskTool] failed.task_id is empty"));
                }
                let mut task = self.task_store.remove_task(result.task_id.as_str()).await;
                let mut task = if let Some(mut t) = task {
                    t.set_result(result.content).set_status(status)
                } else {
                    return Err(anyhow::anyhow!(
                        "[AgentTaskTool] failed task[{}] buf not found .",
                        result.task_id
                    ));
                };
                fae_agent::Hook::agent_call_session_over(
                    &agent_id,
                    &session_id,
                    |_ctx, output| async move {
                        task.set_ext(output);
                        channel.send(task).await?;
                        Ok(())
                    },
                );
            }
        }

        Ok(ToolResponse::with_result(
            "Task update successfully.".into(),
        ))
    }
}
