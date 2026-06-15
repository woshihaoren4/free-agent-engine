use crate::tools::scheduled_execution::SCHEDULED_EXECUTION_TOOL_NAME;
use crate::tools::{ScheduledExecution, ScheduledTask};
use crate::{IdenInfo, Tool};
use async_trait::async_trait;
use chrono::{DateTime, Local};
use cron::Schedule;
use fae_agent::{
    Context, Env, EnvEvent, Environment, Select, Task, TaskResult, TaskType, Thing, ThingItem,
    ThingSelect, TimedTask, ToolRequest, ToolResponse,
};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use wd_tools::PFErr;
use wd_tools::channel::Channel;

pub const CRON_RUNTIME_ID: &str = "FAE_CRON_RUNTIME";

#[derive(Debug, Clone)]
struct CronJob {
    task: ScheduledTask,
    next_time: DateTime<Local>,
    schedule: Schedule,
}

impl PartialEq for CronJob {
    fn eq(&self, other: &Self) -> bool {
        self.next_time == other.next_time
    }
}

impl Eq for CronJob {}

impl PartialOrd for CronJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        // Reverse order so BinaryHeap is a min-heap
        Some(other.next_time.cmp(&self.next_time))
    }
}

impl Ord for CronJob {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse order so BinaryHeap is a min-heap
        other.next_time.cmp(&self.next_time)
    }
}

#[derive(Debug)]
pub struct CronRuntime {
    heap: Arc<RwLock<BinaryHeap<CronJob>>>,
    parent: Option<Env>,
    channel: Channel<ScheduledTask>,
    spawn_channel: Channel<TaskResult>,
}

impl CronRuntime {
    pub fn new() -> Self {
        let channel = Channel::with_cap(100);
        let spawn_channel = Channel::with_cap(8);
        Self {
            heap: Arc::new(RwLock::new(BinaryHeap::new())),
            parent: None,
            channel,
            spawn_channel,
        }
    }

    pub fn get_tool(&self) -> ScheduledExecution {
        ScheduledExecution::new(self.channel.clone())
    }
    pub async fn exec_tool(&self, mut task: Task) -> anyhow::Result<TaskResult> {
        if let Some(req) = task.into_inner::<ToolRequest>() {
            let tool = self.get_tool();
            let iden = IdenInfo::new(
                task.id.clone(),
                task.agent_id.clone(),
                task.user_id.clone(),
                task.ctx.clone(),
            );
            return match tool.call(iden, req.arguments).await {
                Ok(resp) => Ok(TaskResult::success(task.id, task.agent_id).set_data(resp)),
                Err(e) => {
                    let info = format!("Tool[{}] call failed. error: {}", req.tool_name, e);
                    Ok(TaskResult::success(task.id, task.agent_id)
                        .set_data(ToolResponse::with_result(info)))
                }
            };
        } else {
            return anyhow::anyhow!("[ScheduledExecution] task is not a tool request").err();
        }
    }
    pub fn is_scheduled_tool_task(task: &Task) -> bool {
        task.get_type() == &TaskType::Tool
            && task.exec_channel == "default"
            && task.deref_args::<ToolRequest, bool>(|x| {
                if let Some(r) = x {
                    r.tool_name.as_str() == SCHEDULED_EXECUTION_TOOL_NAME
                } else {
                    false
                }
            })
    }
    pub async fn get_next_job_expire_time(&self) -> std::time::Duration {
        let now = Local::now();
        let heap = self.heap.read().await;
        if let Some(job) = heap.peek() {
            let duration = job.next_time.signed_duration_since(now).to_std();
            duration.unwrap_or_else(|_| std::time::Duration::from_millis(0))
        } else {
            std::time::Duration::from_secs(3600 * 24 * 365)
        }
    }
    pub async fn push_job(heap: Arc<RwLock<BinaryHeap<CronJob>>>, job: CronJob) {
        let mut heap = heap.write().await;
        heap.push(job);
    }
    pub async fn pop_job(heap: Arc<RwLock<BinaryHeap<CronJob>>>) -> Option<CronJob> {
        let mut heap = heap.write().await;
        heap.pop()
    }
}

#[async_trait::async_trait]
impl Environment for CronRuntime {
    fn id(&self) -> &'static str {
        CRON_RUNTIME_ID
    }

    async fn register_parent_env(&mut self, env: Env) {
        self.parent = Some(env);
    }

    async fn watch(&self) -> anyhow::Result<EnvEvent> {
        loop {
            let sleep_duration = self.get_next_job_expire_time().await;
            let sleep_fut = tokio::time::sleep(sleep_duration);
            wd_log::log_info_ln!("[CronRuntime] sleep for {:?}s", sleep_duration);
            let recv_fut = self.channel.recv();
            let spawn_fut = self.spawn_channel.recv();
            let heap = self.heap.clone();

            let parent_fut = if let Some(ref p) = self.parent {
                Some(p.watch())
            } else {
                None
            };
            tokio::select! {
                // 接收新的定时任务
                res = recv_fut => {
                    let task = res?;
                    if let Ok(schedule) = Schedule::from_str(&task.cron_expression) {
                        if let Some(next_time) = schedule.upcoming(Local).next() {
                            Self::push_job(heap.clone(), CronJob {task,
                                next_time,
                                schedule,}).await;
                        }
                    }else{
                        wd_log::log_error_ln!("[CronRuntime] Invalid cron expression: {}, remove schedule task:{:?}", task.cron_expression,task);
                    }
                }
                task_result = spawn_fut =>{
                    return Ok(EnvEvent::TaskResult(task_result?));
                }
                _ = sleep_fut => {
                    if let Some(mut job) = Self::pop_job(heap.clone()).await {
                        if !job.task.execute_once {
                            if let Some(next_time) = job.schedule.upcoming(Local).next() {
                                job.next_time = next_time;
                                Self::push_job(heap.clone(), job.clone()).await;
                            }
                        }
                        let event = EnvEvent::Timed(TimedTask{
                            task_content:job.task.task_content,
                            agent_id:job.task.agent_id,
                            session_id:job.task.session_id,
                            user_id:job.task.user_id,
                        });
                        return Ok(event);
                    }
                }
                res = async {
                    if let Some(fut) = parent_fut {
                        fut.await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    return res;
                }
            }
        }
    }

    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>> {
        if let ThingSelect::Tool(chan, name) = &select.select {
            if name == "scheduled_execution" && chan == "default" {
                let tool = self.get_tool();
                use crate::executors::Tool;
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

    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()> {
        let (ts, ptasks): (Vec<_>, Vec<_>) = tasks.into_iter().partition(|t| {
            t.get_type() == &TaskType::Tool
                && t.exec_channel == "default"
                && Self::is_scheduled_tool_task(t)
        });
        if !ptasks.is_empty() {
            if let Some(ref p) = self.parent {
                p.spawn(ptasks).await?;
            } else {
                return Err(anyhow::anyhow!("[CronRuntime] spawn failed, no parent"));
            }
        }
        for i in ts {
            let result = self.exec_tool(i).await?;
            let chan = self.spawn_channel.clone();
            tokio::spawn(async move {
                if let Err(e) = chan.send(result).await {
                    wd_log::log_error_ln!("[CronRuntime] send task result failed: {:?}", e);
                }
            });
        }
        Ok(())
    }

    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult> {
        if task.get_type() == &TaskType::Tool
            && task.exec_channel == "default"
            && Self::is_scheduled_tool_task(&task)
        {
            return self.exec_tool(task).await;
        }
        if let Some(ref p) = self.parent {
            p.execute(task).await
        } else {
            Err(anyhow::anyhow!("[CronRuntime] execute failed, no parent"))
        }
    }
}
