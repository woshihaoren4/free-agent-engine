use crate::tools::{ScheduledExecution, ScheduledTask};
use async_trait::async_trait;
use cron::Schedule;
use fae_agent::{
    Env, EnvEvent, Environment, Select, Task, TaskResult, TaskType, Thing, ThingItem, ThingSelect,
    Context
};
use std::collections::BinaryHeap;
use std::cmp::Ordering;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use chrono::{DateTime, Utc};
use wd_tools::channel::Channel;

pub const CRON_RUNTIME_ID: &str = "FAE_CRON_RUNTIME";

#[derive(Clone)]
struct CronJob {
    task: ScheduledTask,
    next_time: DateTime<Utc>,
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
    parent: Option<Env>,
    channel: Channel<ScheduledTask>,
}

impl CronRuntime {
    pub fn new() -> Self {
        let channel = Channel::with_cap(100);
        Self {
            parent: None,
            channel,
        }
    }

    pub fn get_tool(&self) -> ScheduledExecution {
        ScheduledExecution::new(self.channel.clone())
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
        let mut heap: BinaryHeap<CronJob> = BinaryHeap::new();
        
        loop {
            let now = Utc::now();
            let sleep_duration = if let Some(job) = heap.peek() {
                let duration = job.next_time.signed_duration_since(now).to_std();
                match duration {
                    Ok(d) => d,
                    Err(_) => std::time::Duration::from_millis(0), // already passed
                }
            } else {
                std::time::Duration::from_secs(3600 * 24 * 365) // wait practically forever
            };

            let sleep_fut = tokio::time::sleep(sleep_duration);
            let recv_fut = self.channel.recv();
            let parent_fut = if let Some(ref p) = self.parent {
                Some(p.watch())
            } else {
                None
            };

            tokio::select! {
                res = recv_fut => {
                    match res {
                        Ok(task) => {
                            if let Ok(schedule) = Schedule::from_str(&task.cron_expression) {
                                if let Some(next_time) = schedule.upcoming(Utc).next() {
                                    heap.push(CronJob {
                                        task,
                                        next_time,
                                        schedule,
                                    });
                                }
                            }
                        }
                        Err(_) => {
                            // Channel closed, should not happen as long as runtime holds the tool
                        }
                    }
                }
                _ = sleep_fut => {
                    if let Some(mut job) = heap.pop() {
                        // Time to execute the job
                        if let Some(ref p) = self.parent {
                            let mut ctx = Context::new(p.clone());
                            ctx.set(fae_agent::GLOBAL_KEY_AGENT_ID.to_string(), job.task.agent_id.clone());
                            ctx.set(fae_agent::GLOBAL_KEY_PLAN_ID.to_string(), job.task.plan_id.clone());
                            
                            let mut t = Task::with_content(ctx);
                            t = t.set_type(TaskType::Model)
                                 .set_args(job.task.task_content.clone())
                                 .set_agent_id(job.task.agent_id.clone())
                                 .set_user_id(job.task.user_id.clone());
                            t.id = job.task.plan_id.clone();
                            
                            if let Err(e) = p.spawn(vec![t]).await {
                                wd_log::log_error_ln!("[CronRuntime] failed to spawn scheduled task: {}", e);
                            }
                        }

                        // Re-schedule if not execute_once
                        if !job.task.execute_once {
                            if let Some(next_time) = job.schedule.upcoming(Utc).next() {
                                job.next_time = next_time;
                                heap.push(job);
                            }
                        }
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
                return Ok(vec![Thing::new(self.id().to_string()).add_item(ThingItem::Tool(
                    tool.description().to_string(),
                    tool.arguments(),
                )).into_self()]);
            }
        }
        
        if let Some(ref p) = self.parent {
            p.query(select).await
        } else {
            Ok(Vec::new())
        }
    }

    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()> {

        if let Some(ref p) = self.parent {
            p.spawn(tasks).await
        } else {
            Err(anyhow::anyhow!("[CronRuntime] spawn failed, no parent"))
        }
    }

    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult> {
        if let Some(ref p) = self.parent {
            p.execute(task).await
        } else {
            Err(anyhow::anyhow!("[CronRuntime] execute failed, no parent"))
        }
    }
}
