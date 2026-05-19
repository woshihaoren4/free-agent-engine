use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, RwLock};
use wd_tools::channel::Channel;
use wd_tools::{PFArc, PFErr};
use fae_agent::{Env, EnvEvent, Environment, Planning, PlanningResult, Task, TaskResult, TaskType, Thing, ThingSelect};

const DEFAULT_PLAN_RUNTIME_ID: &str = "FAE_DEFAULT_PLAN_RUNTIME";
const DEFAULT_PLAN_RUNTIME_EXEC_CHANNEL: &str = "default";
const DEFAULT_PLAN_RUNTIME_EVENT_CHANNEL_COUNT: usize = 1024;
const DEFAULT_PLAN_RUNTIME_PLAN_ID_PREFIX: &str = "__PRSUB_";

pub struct PlanCtl{
    task : Task,
    plan: Box<dyn Planning + Send + 'static>,
    notify: Option<Arc<Notify>>,
    result: Option<TaskResult>,
}
impl PlanCtl {
    pub fn into_task_result(&mut self) -> (Task, Option<TaskResult>) {
        (std::mem::take(&mut self.task), std::mem::replace(&mut self.result,None))
    }
}

pub struct PlanRuntime{
    events: Channel<EnvEvent>,
    parent: Option<Env>,
    plans: RwLock<HashMap<String,Arc<Mutex<PlanCtl>>>>,
}

impl PlanRuntime {
    pub fn new() -> Self {
        let events = Channel::with_cap(DEFAULT_PLAN_RUNTIME_EVENT_CHANNEL_COUNT);
        let plans = RwLock::new(HashMap::new());
        Self {
            events,
            parent: None,
            plans,
        }
    }
    fn generate_plan_sub_id(id:&str) -> String {
        format!("{}{}",DEFAULT_PLAN_RUNTIME_PLAN_ID_PREFIX,id)
    }
    async fn push_plan(&self,id:String,plan:Arc<Mutex<PlanCtl>>) {
        let mut plans = self.plans.write().await;
        plans.insert(id,plan);
    }
    async fn run_plan(&self,plan:PlanCtl,init_result:PlanningResult) {
        let pid = Self::generate_plan_sub_id(&plan.task.agent_id);
        let env = self.parent.clone().unwrap();
        let channel = if init_result.is_end() {
            Some(self.events.clone())
        }else{
            None
        };
        let p = Arc::new(Mutex::new(plan));
        self.push_plan(pid,p.clone()).await;
        tokio::spawn(async move {
            if let PlanningResult::End(opt) = init_result {
                if let Some(result) = opt {
                    if let Some(channel) = channel {
                        if let Err(e) = channel.send(EnvEvent::TaskResult(result)).await{
                            wd_log::log_error_ln!("[PlanRuntime:run_plan] send task result to channel failed: {:?}",e);
                        }
                    }
                }
            }else if let PlanningResult::Tasks(tasks) = init_result {
                if let Err(e) = env.spawn(tasks).await{
                    wd_log::log_error_ln!("[PlanRuntime:run_plan] spawn tasks failed: {:?}",e);
                }
            }
        });
    }
    async fn remove_plan(&self,id:&str) -> Option<Arc<Mutex<PlanCtl>>> {
        let mut plans = self.plans.write().await;
        plans.remove(id)
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
        let mut ret = false;
        loop {
            ret = false;
            let self_fut = self.events.recv();
            if let Some(ref p) = self.parent {
                let parent_fut = p.watch();
                let event = tokio::select! {
                e = self_fut => e?,
                e = parent_fut => {ret = true; e?},
            };
            if ret {
                return Ok(event);
            }
                todo!() // 处理self_fut事件
            }else{
                let e= self_fut.await?;
                todo!() //处理self_fut事件
            }
        }
    }

    async fn query(&self, select: ThingSelect) -> anyhow::Result<Vec<Thing>> {
        if let ThingSelect::Plan(id) = select {
            //todo ，根据id返回内容
            return Ok(Vec::new());
        }else if let Some(ref p) = self.parent {
            return p.query(select).await
        }else{
            return Ok(Vec::new());
        }
    }

    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()> {
        //plan_runtime 只处理plan任务，其他任务直接传递给父环境处理
        if self.parent.is_none() {
            return anyhow::anyhow!("[PlanRuntime:spawn] Cannot spawn tasks because parent is None").err();
        }
        //先过滤检查
        let mut plans = vec![];
        let mut ptasks = vec![];
        for t in tasks {
            if t.r#type == TaskType::Plan && (t.get_exec_channel().is_empty() || t.get_exec_channel() == DEFAULT_PLAN_RUNTIME_EXEC_CHANNEL) {
                plans.push(t);
            }else{
                ptasks.push(t);
            }
        }
        let mut ps = vec![];
        for mut task in plans {
            let mut plan = if let Some(s) = task.into_inner::<Box<dyn Planning + Send + 'static>>() {
                s
            }else{
                return anyhow::anyhow!("[PlanRuntime:spawn] this is not a plan: {:?}", task).err();
            };
            let plan_result = plan.init().await?;
            ps.push((PlanCtl{
                plan,
                task,
                notify: None,
                result: None,
            },plan_result));
        }
        //先执行父任务
        if !ptasks.is_empty() {
            if let Some(ref s) = self.parent {
                s.spawn(ptasks).await?;
            }else{
                // 无法执行任务，立刻报错
                return anyhow::anyhow!("[PlanRuntime:spawn] task executor not found: {:?}", ptasks).err();
            }
        }
        //再执行子任务
        for (p,r) in ps {
            self.run_plan(p,r).await;
        }
        Ok(())
    }

    async fn execute(&self, mut task: Task) -> anyhow::Result<TaskResult> {
        let mut plan = if let Some(s) = task.into_inner::<Box<dyn Planning + Send + 'static>>() {
            s
        }else{
            return anyhow::anyhow!("[PlanRuntime:spawn] this is not a plan: {:?}", task).err();
        };
        let pid = Self::generate_plan_sub_id(task.agent_id.as_str());

        let tasks = match plan.init().await?{
            PlanningResult::End(opt) => {
                // 任务完成，返回结果
                return if let Some(result) = opt {
                     Ok(result)
                }else{
                    Ok(TaskResult::success(task.id,task.agent_id))
                }
            }
            PlanningResult::Tasks(tasks) => tasks,
        };
        let notify = Notify::new().arc();
        let plan_ctl = PlanCtl{
            plan,
            task,
            notify: Some(notify.clone()),
            result: None,
        };
        self.run_plan(plan_ctl,PlanningResult::Tasks(tasks)).await;
        notify.notified().await;
        if let Some(plan) =self.remove_plan(pid.as_str()).await{
            let mut p = plan.lock().await;
            let (task, result) = p.into_task_result();
            drop(p);
            if let Some(result) = result {
                Ok(result)
            }else{
                Ok(TaskResult::success(task.id, task.agent_id))
            }
        }else{
            return anyhow::anyhow!("[PlanRuntime:execute] plan not found: {:?}", pid).err();
        }
    }
}
