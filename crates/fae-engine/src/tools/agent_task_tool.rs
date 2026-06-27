use crate::agent_runtime::AgentTaskStore;
use crate::{IdenInfo, Tool};
use fae_agent::{AgentInfo, AgentTask, AgentTaskStatus, GLOBAL_KEY_AGENT_ID, GLOBAL_KEY_SESSION_ID, ToolResponse, AgentTasks};
use serde_json::Value;
use std::sync::Arc;
use wd_tools::channel::Channel;

pub const AGENT_TASK_TOOL_NAME: &str = "agent_exec_task";

#[derive(Debug)]
pub struct AgentTaskTool {
    chan: Channel<AgentTasks>,
    pub task_store: Arc<dyn AgentTaskStore + Send + Sync + 'static>,
}

impl AgentTaskTool {
    pub fn new(
        chan: Channel<AgentTasks>,
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

    async fn call(&self, mut iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let agent_id = iden
            .get(GLOBAL_KEY_AGENT_ID)
            .unwrap_or(iden.get_agent_id().to_string());
        let session_id = iden.get(GLOBAL_KEY_SESSION_ID).unwrap_or("".to_string());
        let channel = self.chan.clone();
        let task: AgentTaskStatus = serde_json::from_str(&args)
            .map_err(|e| anyhow::anyhow!("[AgentTaskTool] invalid arguments: {}", e))?;
        let _task_id = task.get_task_id().to_string();
        let status = task.status().to_string();
        let mut result_content = "Task update successfully.".to_string();
        match task {
            AgentTaskStatus::Create(mut create) => {
                result_content.push_str("\nPlease wait for me to complete this task, and I will notify you when it is finished.");
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
                    create.executor.user_id = author.user_id.clone();
                }
                let mut task = AgentTask::default()
                    .set_task_id(wd_tools::uuid::v4())
                    .set_status(status)
                    .set_author(author)
                    .set_executor(create.executor)
                    .set_content(create.content);
                task.update_timestamp();
                self.task_store.push_task(task.clone()).await;
                if let Some(s) = iden.ctx.get_output(){
                    task.set_ext(s);
                }
                //挂钩子，等当前agent执行完成，则开始执行任务
                fae_agent::Hook::agent_call_session_over(
                    &agent_id,
                    &session_id,
                     |ctx, over| {
                        over.add_sub_task();
                         async move {
                             channel.send(task.into()).await?;
                             Ok(())
                         }
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
                result_content.push_str("\nThe results have been sent to the task publisher; you can end your task now.");
                //移除任务
                if result.task_id.is_empty() {
                    return Err(anyhow::anyhow!(
                        "[AgentTaskTool] completed.task_id is empty"
                    ));
                }
                //移除任务
                let ext = iden.ctx.get_output();
                let tasks = self.task_store.complete_task(result.task_id.as_str(),status,result.content,ext).await?;
                if !tasks.is_empty(){
                    fae_agent::Hook::agent_call_session_over(
                        &agent_id,
                        &session_id,
                        |ctx, over| {
                            over.add_sub_task();
                            async move {
                                channel.send(tasks).await?;
                                Ok(())
                            }},
                    );
                }

            }
            AgentTaskStatus::Failed(result) => {
                result_content.push_str("\nThe results have been sent to the task publisher; you can end your task now.");
                //移除任务
                if result.task_id.is_empty() {
                    return Err(anyhow::anyhow!("[AgentTaskTool] failed.task_id is empty"));
                }
                let tasks = self.task_store.complete_task(result.task_id.as_str(),status,result.content,iden.ctx.get_output()).await?;
                if !tasks.is_empty(){
                    fae_agent::Hook::agent_call_session_over(
                        &agent_id,
                        &session_id,
                        |ctx, over| {
                            over.add_sub_task();
                            async move {
                                channel.send(tasks).await?;
                                Ok(())
                            }},
                    );
                }
            }
        }

        Ok(ToolResponse::with_result(
            result_content,
        ))
    }
}
