use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, RwLock};
use wd_tools::channel::Channel;
use wd_tools::PFErr;
use fae_agent::{Env, EnvEvent, Environment, Planning, Task, TaskResult, TaskType, Thing, ThingSelect};

const DEFAULT_PLAN_RUNTIME_ID: &str = "FAE_DEFAULT_PLAN_RUNTIME";
const DEFAULT_PLAN_RUNTIME_EXEC_CHANNEL: &str = "default";
const DEFAULT_PLAN_RUNTIME_EVENT_CHANNEL_COUNT: usize = 1024;
const DEFAULT_PLAN_RUNTIME_PLAN_ID_PREFIX: &str = "__PRSUB_";

pub struct PlanCtl{
    task : Task,
    plan: Box<dyn Planning + Send + 'static>,
    notify: Option<Notify>
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
    async fn run_plan(&self,plan:PlanCtl) {
        //todo 启动
        //插入
        let pid = Self::generate_plan_sub_id(&plan.task.id);
        let p = Arc::new(Mutex::new(plan));
        self.push_plan(pid,p).await;
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
            let plan = if let Some(s) = task.into_inner::<Box<dyn Planning + Send + 'static>>() {
                s
            }else{
                return anyhow::anyhow!("[PlanRuntime:spawn] this is not a plan: {:?}", task).err();
            };
            ps.push(PlanCtl{
                plan,
                task,
                notify: None,
            });
        }
        //先执行父任务
        if !ptasks.is_empty() {
            if let Some(s) = self.parent {
                s.spawn(ptasks).await
            }else{
                // 无法执行任务，立刻报错
                return anyhow::anyhow!("[PlanRuntime:spawn] task executor not found: {:?}", ptasks).err();
            }
        };
        //再执行子任务
        todo!()
    }

    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult> {
        todo!()
    }
}
