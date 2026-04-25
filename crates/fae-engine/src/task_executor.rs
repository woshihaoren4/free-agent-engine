use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use wd_tools::channel::Channel;
use wd_tools::PFErr;
use fae_agent::{Env, EnvEvent, Environment, Task, TaskExecutor, TaskResult, TaskType, Thing, ThingItem, ThingSelect};

pub struct TaskRuntime {
    events: Channel<EnvEvent>,
    parent: Option<Env>,
    executors: HashMap<TaskType, Arc<dyn TaskExecutor+Send+'static>>,
}

impl TaskRuntime {
    pub fn new() -> Self {
        let events = Channel::with_cap(1024);
        Self { events,  parent: None, executors: HashMap::new()}
    }
    pub fn raw_register_executor(&mut self, task_type: TaskType, executor: Arc<dyn TaskExecutor+Send+'static>) {
        self.executors.insert(task_type, executor);
    }
    pub fn register_executor<T: TaskExecutor+Send+'static>(&mut self, task_type: TaskType, executor:T) {
        self.raw_register_executor(task_type, Arc::new(executor));
    }
    pub async fn exec(executor: Arc<dyn TaskExecutor+Send>, task: Task) -> TaskResult {
        let task_id = task.id.clone();
        let agent_id = task.agent_id.clone();
        match executor.execute(task).await{
            Ok(result) => result,
            Err(err) => {
                TaskResult::new(fae_agent::error::TASK_ERROR_CODE_UNKNOWN,format!("{:?}", err),task_id,agent_id)
            },
        }
    }
}

#[async_trait::async_trait]
impl Environment for TaskRuntime{
    fn id(&self) -> &'static str {
        "FAE_DEFAULT_TASK_EXECUTOR"
    }

    async fn register_parent_env(&mut self, env: Env) {
        self.parent = Some(env);
    }

    async fn watch(&self) -> EnvEvent {
        if let Some(ref parent_env) = self.parent{
            return parent_env.watch().await
        }
        EnvEvent::None
    }

    async fn query(&self, select: ThingSelect) -> anyhow::Result<Vec<Thing>> {
        //为空时，查询所有任务执行器
        if select.is_none() {
            let items = self.executors.iter().map(|(_,e)|ThingItem::Executor(e.desc())).collect();
            let thing = Thing::new(self.id().to_string()).set_items(items).into_self();
            return Ok(vec![thing]);
        }
        //根据任务类型查询
        if let ThingSelect::Executor(ref task_type) = select {
            if let Some(e) = self.executors.get(task_type) {
                return Ok(vec![Thing::new(self.id().to_string()).add_item(ThingItem::Executor(e.desc())).into_self()]);
            }
        }
        //如果父环境也没有，就返回空
        if let Some(e) = self.parent.as_ref() {
            return e.query(select).await
        }
        Ok(vec![])
    }

    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()> {
        //先检查
        for i in &tasks {
            let list = self.query(ThingSelect::Executor(i.r#type.clone())).await?;
            if list.is_empty() {
                return anyhow::anyhow!("[TaskRuntime:spawn]task executor not found: {:?}", i.r#type).err();
            }
        }
        //后执行
        for task in tasks {
            if let Some(e) = self.executors.get(&task.r#type) {
                //优先自己执行
                let events_channel = self.events.clone();
                let executor = e.clone();
                tokio::spawn(async move {
                    let result = Self::exec(executor, task).await;
                    if let Err(e) = events_channel.send(EnvEvent::TaskResult(result)).await{
                        wd_log::log_error_ln!("[TaskRuntime:spawn] send task result error: {:?}",e);
                    };
                });
            }else if let Some(e) = self.parent.as_ref() {
                //再委托给父环境
                e.spawn(vec![task]).await?;
            }else{
                //如果父环境也没有，就报错
                return anyhow::anyhow!("[TaskRuntime:spawn] task executor not found: {:?}", task.r#type).err();
            }
        }
        Ok(())
    }

    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult> {
        if let Some(e) = self.executors.get(&task.r#type) {
            //优先自己执行
            let result = Self::exec(e.clone(), task).await;
            Ok(result)
        }else if let Some(e) = self.parent.as_ref() {
            //再委托给父环境
            e.execute(task).await
        }else{
            //如果父环境也没有，就报错
            return anyhow::anyhow!("[TaskRuntime:execute] task executor not found: {:?}", task.r#type).err();
        }
    }
}


// TaskRuntimeRef is a reference to TaskRuntime.
#[derive(Clone)]
pub struct TaskRuntimeRef(Arc<TaskRuntime>);
impl TaskRuntimeRef {
    pub fn as_env(&self) -> Env {
        Env::from(self.0.clone() as Arc<dyn Environment+Send+'static>)
    }
}
impl From<TaskRuntime> for TaskRuntimeRef {
    fn from(runtime: TaskRuntime) -> Self {Self(Arc::new(runtime))}
}
impl Deref for TaskRuntimeRef {
    type Target = TaskRuntime;
    fn deref(&self) -> &Self::Target {&self.0}
}