use fae_agent::error::{
    TASK_ERROR_CODE_PLAN_ABORT, TASK_ERROR_CODE_PLAN_ABORT_EXTERNAL,
    TASK_ERROR_CODE_PLAN_ABORT_USER,
};
use fae_agent::{
    EndPlanTaskArgs, Env, EnvEvent, Environment, GLOBAL_KEY_AGENT_ID, Planning, PlanningResult,
    Select, Task, TaskResult, TaskType, Thing, ThingItem, ThingSelect,
};
use std::collections::HashMap;
use std::ops::DerefMut;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, RwLock};
use wd_tools::channel::Channel;
use wd_tools::{PFArc, PFErr};

pub const DEFAULT_PLAN_RUNTIME_ID: &str = "FAE_DEFAULT_PLAN_RUNTIME";
pub const DEFAULT_PLAN_RUNTIME_EXEC_CHANNEL: &str = "default";
pub const DEFAULT_PLAN_RUNTIME_EVENT_CHANNEL_COUNT: usize = 1024;
pub const DEFAULT_PLAN_RUNTIME_PLAN_ID_PREFIX: &str = "__PRSUB_";
pub const DEFAULT_PLAN_RUNTIME_PLAN_ID_SPILT: &str = "_*P#R$S@U&B_";

#[derive(Debug)]
pub struct PlanCtl {
    task: Task,
    plan: Box<dyn Planning + Send + 'static>,
    notify: Option<Arc<Notify>>,
    result: Option<TaskResult>,
}
impl PlanCtl {
    pub fn into_task_result(&mut self) -> (Task, Option<TaskResult>) {
        let task = Task::none();
        (
            std::mem::replace(&mut self.task, task),
            std::mem::replace(&mut self.result, None),
        )
    }
}
#[derive(Debug)]
pub struct PlanRuntime {
    events: Channel<EnvEvent>,
    parent: Option<Env>,
    plans: Arc<RwLock<HashMap<String, Arc<Mutex<PlanCtl>>>>>,
}

impl PlanRuntime {
    pub fn new() -> Self {
        let events = Channel::with_cap(DEFAULT_PLAN_RUNTIME_EVENT_CHANNEL_COUNT);
        let plans = RwLock::new(HashMap::new());
        Self {
            events,
            parent: None,
            plans: Arc::new(plans),
        }
    }
    fn generate_plan_sub_id(tid: &str, aid: &str) -> String {
        format!(
            "{}{}{}{}",
            DEFAULT_PLAN_RUNTIME_PLAN_ID_PREFIX, tid, DEFAULT_PLAN_RUNTIME_PLAN_ID_SPILT, aid
        )
    }
    fn parse_plan_sub_id(id: &str) -> Option<(String, String)> {
        if id.starts_with(DEFAULT_PLAN_RUNTIME_PLAN_ID_PREFIX) {
            let ss = id
                .trim_start_matches(DEFAULT_PLAN_RUNTIME_PLAN_ID_PREFIX)
                .split(DEFAULT_PLAN_RUNTIME_PLAN_ID_SPILT)
                .collect::<Vec<_>>();
            if ss.len() != 2 {
                return None;
            }
            return Some((ss[0].to_owned(), ss[1].to_owned()));
        } else {
            None
        }
    }
    async fn push_plan(&self, id: String, plan: Arc<Mutex<PlanCtl>>) {
        let mut plans = self.plans.write().await;
        plans.insert(id, plan);
    }
    async fn get_plan(&self, id: &str) -> Option<Arc<Mutex<PlanCtl>>> {
        Self::get_plan_raw(&self.plans, id).await
    }
    async fn get_plan_raw(
        ptr: &Arc<RwLock<HashMap<String, Arc<Mutex<PlanCtl>>>>>,
        id: &str,
    ) -> Option<Arc<Mutex<PlanCtl>>> {
        ptr.read().await.get(id).cloned()
    }
    async fn run_plan(&self, mut plan: PlanCtl, init_result: PlanningResult) {
        let pid = Self::generate_plan_sub_id(&plan.task.id, &plan.task.agent_id);
        let env = self.parent.clone().unwrap();
        let channel = if init_result.is_end() {
            Some(self.events.clone())
        } else {
            None
        };
        plan.task
            .set(GLOBAL_KEY_AGENT_ID, plan.task.agent_id.clone());
        let p = Arc::new(Mutex::new(plan));
        self.push_plan(pid.clone(), p.clone()).await;
        tokio::spawn(async move {
            if let PlanningResult::End(opt) = init_result {
                if let Some(result) = opt {
                    if let Some(channel) = channel {
                        if let Err(e) = channel.send(EnvEvent::TaskResult(result)).await {
                            wd_log::log_error_ln!(
                                "[PlanRuntime:run_plan] send task result to channel failed: {:?}",
                                e
                            );
                        }
                    }
                }
            } else if let PlanningResult::Tasks(mut tasks) = init_result {
                tasks.iter_mut().for_each(|task| {
                    task.agent_id = pid.clone();
                });
                if let Err(e) = env.spawn(tasks).await {
                    wd_log::log_error_ln!("[PlanRuntime:run_plan] spawn tasks failed: {:?}", e);
                }
            }
        });
    }
    async fn remove_plan(&self, id: &str) -> Option<Arc<Mutex<PlanCtl>>> {
        Self::remove_plan_raw(&self.plans, id).await
    }
    async fn remove_plan_raw(
        ptr: &Arc<RwLock<HashMap<String, Arc<Mutex<PlanCtl>>>>>,
        id: &str,
    ) -> Option<Arc<Mutex<PlanCtl>>> {
        let mut plans = ptr.write().await;
        plans.remove(id)
    }
    // 异常处理
    async fn abort_plan_by_error<S: Into<String>>(
        code: i32,
        plan: &mut PlanCtl,
        info: S,
        remove_id: &mut String,
    ) {
        plan.plan.abort().await;
        plan.result = Option::from(TaskResult::error(
            code,
            info.into(),
            plan.task.id.as_str(),
            plan.task.agent_id.as_str(),
        ));
        //通知计划执行器
        if let Some(ref notify) = plan.notify {
            notify.notify_one();
            *remove_id = String::new();
        } else {
            wd_log::log_error_ln!(
                "[PlanRuntime:abort_plan_by_error] plan[{}] result: {:?}",
                plan.task.id,
                plan.result
            );
        }
    }
    async fn abort_plan_by_id(
        &self,
        pid: String,
        aid: String,
        reason: String,
    ) -> anyhow::Result<()> {
        let mut id = Self::generate_plan_sub_id(pid.as_str(), aid.as_str());
        let plan = if let Some(p) = self.get_plan(&id).await {
            p
        } else {
            return Err(anyhow::anyhow!("[PlanRuntime] not found plan, id={}", id));
        };
        let mut plan_lock = plan.lock().await;
        Self::abort_plan_by_error(
            TASK_ERROR_CODE_PLAN_ABORT_USER,
            plan_lock.deref_mut(),
            reason,
            &mut id,
        )
        .await;
        if !id.is_empty() {
            self.remove_plan(&id).await;
        }
        Ok(())
    }
    fn task_result_callback(&self, mut result: TaskResult) {
        let plans = self.plans.clone();
        let chan = self.events.clone();
        let env = self.parent.clone();
        tokio::spawn(async move {
            let mut remove_id = "".to_string();
            let plan = Self::get_plan_raw(&plans, result.agent_id.as_str()).await;
            let plan = if let Some(p) = plan {
                p
            } else {
                wd_log::log_error_ln!(
                    "[PlanRuntime:task_result_callback] plan not found: {:?}",
                    result.agent_id
                );
                return;
            };
            let mut plan_lock = plan.lock().await;
            remove_id = plan_lock.task.agent_id.clone();
            std::mem::swap(&mut remove_id, &mut result.agent_id);
            let next_result = plan_lock.plan.next(result).await;
            match next_result {
                Ok(PlanningResult::End(opt)) => {
                    let notify = plan_lock.notify.clone();
                    if let Some(notify) = notify {
                        // 如果存在待通知折，则必须通知，并且不得移除结果
                        plan_lock.result = opt;
                        notify.notify_one();
                        remove_id = String::new();
                    } else if let Some(r) = opt {
                        // 存在待通知的结果，返回结果给等待者
                        if let Err(e) = chan.send(EnvEvent::TaskResult(r)).await {
                            Self::abort_plan_by_error(TASK_ERROR_CODE_PLAN_ABORT_EXTERNAL, &mut plan_lock, format!("[PlanRuntime:task_result_callback] plan execute success. but send task result to channel failed: {:?}", e), &mut remove_id).await;
                        }
                    } else {
                        //异步且没有返回值，则不进行回调通知，正常结束任务即可
                    }
                }
                Ok(PlanningResult::Tasks(mut tasks)) => {
                    // 存在子任务，需要继续执行
                    tasks.iter_mut().for_each(|task| {
                        task.agent_id = remove_id.clone();
                    });
                    if let Some(env) = env {
                        if let Err(e) = env.spawn(tasks).await {
                            Self::abort_plan_by_error(
                                TASK_ERROR_CODE_PLAN_ABORT_EXTERNAL,
                                &mut plan_lock,
                                e.to_string(),
                                &mut remove_id,
                            )
                            .await;
                        } else {
                            //下一步执行成功，不移除计划
                            remove_id = String::new();
                        }
                    } else {
                        Self::abort_plan_by_error(
                            TASK_ERROR_CODE_PLAN_ABORT_EXTERNAL,
                            &mut plan_lock,
                            "[PlanRuntime:task_result_callback] parent is nil.",
                            &mut remove_id,
                        )
                        .await;
                    }
                }
                Err(e) => {
                    Self::abort_plan_by_error(
                        TASK_ERROR_CODE_PLAN_ABORT,
                        &mut plan_lock,
                        e.to_string(),
                        &mut remove_id,
                    )
                    .await;
                }
            }
            if !remove_id.is_empty() {
                Self::remove_plan_raw(&plans, &remove_id).await;
            }
        });
    }
}

#[async_trait::async_trait]
impl Environment for PlanRuntime {
    fn id(&self) -> &'static str {
        DEFAULT_PLAN_RUNTIME_ID
    }

    async fn register_parent_env(&mut self, env: Env) {
        self.parent = Some(env);
    }

    async fn watch(&self) -> anyhow::Result<EnvEvent> {
        loop {
            let self_fut = self.events.recv();
            let event = if let Some(ref p) = self.parent {
                let parent_fut = p.watch();
                let event = tokio::select! {
                    e = self_fut => e?,
                    e = parent_fut => e?,
                };

                event
            } else {
                self_fut.await?
            };
            //检查是否需要处理自身事件
            let task_result = if let EnvEvent::TaskResult(tr) = event {
                tr
            } else {
                return Ok(event);
            };
            let (_tid, _aid) = if let Some(id) = Self::parse_plan_sub_id(&task_result.agent_id) {
                id
            } else {
                return Ok(EnvEvent::TaskResult(task_result));
            };
            self.task_result_callback(task_result);
        }
    }

    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>> {
        return if let ThingSelect::Plan(pid, aid) = select.select {
            let id = Self::generate_plan_sub_id(pid.as_str(), aid.as_str());
            if let Some(p) = self.get_plan(&id).await {
                let p = p.lock().await;
                let info = p.plan.debug().await;
                return Ok(vec![
                    Thing::new(self.id().to_string())
                        .add_item(ThingItem::Plan(info))
                        .into_self(),
                ]);
            }
            Ok(Vec::new())
        } else if let Some(ref p) = self.parent {
            p.query(select).await
        } else {
            Ok(Vec::new())
        };
    }

    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()> {
        //plan_runtime 只处理plan任务，其他任务直接传递给父环境处理
        if self.parent.is_none() {
            return anyhow::anyhow!(
                "[PlanRuntime:spawn] Cannot spawn tasks because parent is None"
            )
            .err();
        }
        //先过滤检查
        let mut plans = vec![];
        let mut ptasks = vec![];
        for t in tasks {
            if t.r#type == TaskType::Plan
                && (t.get_exec_channel().is_empty()
                    || t.get_exec_channel() == DEFAULT_PLAN_RUNTIME_EXEC_CHANNEL)
            {
                plans.push(t);
            } else {
                ptasks.push(t);
            }
        }
        let mut ps = vec![];
        for mut task in plans {
            let mut plan = if let Some(s) = task.into_inner::<Box<dyn Planning + Send + 'static>>()
            {
                s
            } else if let Some(abort_plan) = task.into_inner::<EndPlanTaskArgs>() {
                self.abort_plan_by_id(abort_plan.plan_id, abort_plan.agent_id, abort_plan.reason)
                    .await?;
                continue;
            } else {
                return anyhow::anyhow!("[PlanRuntime:spawn] this is not a plan: {:?}", task).err();
            };
            let plan_result = plan.init().await?;
            ps.push((
                PlanCtl {
                    plan,
                    task,
                    notify: None,
                    result: None,
                },
                plan_result,
            ));
        }
        //先执行父任务
        if !ptasks.is_empty() {
            if let Some(ref s) = self.parent {
                s.spawn(ptasks).await?;
            } else {
                // 无法执行任务，立刻报错
                return anyhow::anyhow!(
                    "[PlanRuntime:spawn] task executor not found: {:?}",
                    ptasks
                )
                .err();
            }
        }
        //再执行子任务
        for (p, r) in ps {
            self.run_plan(p, r).await;
        }
        Ok(())
    }

    async fn execute(&self, mut task: Task) -> anyhow::Result<TaskResult> {
        if task.r#type != TaskType::Plan {
            if let Some(ref p) = self.parent {
                return p.execute(task).await;
            } else {
                return anyhow::anyhow!(
                    "[PlanRuntime:execute] task executor not found: {:?}",
                    task
                )
                .err();
            }
        }
        let mut plan = if let Some(s) = task.into_inner::<Box<dyn Planning + Send + 'static>>() {
            s
        } else if let Some(abort_plan) = task.into_inner::<EndPlanTaskArgs>() {
            self.abort_plan_by_id(abort_plan.plan_id, abort_plan.agent_id, abort_plan.reason)
                .await?;
            return Ok(TaskResult::success(task.id, task.agent_id));
        } else {
            return anyhow::anyhow!("[PlanRuntime:spawn] this is not a plan: {:?}", task).err();
        };
        let pid = Self::generate_plan_sub_id(task.id.as_str(), task.agent_id.as_str());

        let mut tasks = match plan.init().await? {
            PlanningResult::End(opt) => {
                // 任务完成，返回结果
                return if let Some(result) = opt {
                    Ok(result)
                } else {
                    Ok(TaskResult::success(task.id, task.agent_id))
                };
            }
            PlanningResult::Tasks(tasks) => tasks,
        };
        let notify = Notify::new().arc();
        let plan_ctl = PlanCtl {
            plan,
            task,
            notify: Some(notify.clone()),
            result: None,
        };
        self.run_plan(plan_ctl, PlanningResult::Tasks(tasks)).await;
        notify.notified().await;
        if let Some(plan) = self.remove_plan(pid.as_str()).await {
            let mut p = plan.lock().await;
            let (task, result) = p.into_task_result();
            drop(p);
            if let Some(result) = result {
                Ok(result)
            } else {
                Ok(TaskResult::success(task.id, task.agent_id))
            }
        } else {
            return anyhow::anyhow!("[PlanRuntime:execute] plan not found: {:?}", pid).err();
        }
    }
}
