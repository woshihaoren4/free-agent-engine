use crate::ToolExecutor;
use crate::executors::ModelOpenAIApiExecutor;
use fae_agent::{Env, EnvEvent, Environment, Select, Task, TaskExecutor, TaskExecutorExt, TaskExecutorExtImpl, TaskResult, TaskType, Thing, ThingItem, ThingSelect};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use wd_tools::PFErr;
use wd_tools::channel::Channel;

const DEFAULT_TASK_RUNTIME_ID: &str = "FAE_DEFAULT_TASK_EXECUTOR";
const DEFAULT_TASK_RUNTIME_EVENT_CHANNEL_COUNT: usize = 1024;

pub struct TaskRuntime {
    events: Channel<EnvEvent>,
    parent: Option<Env>,
    executors: HashMap<String, Arc<dyn TaskExecutor + Send + 'static>>,
}

impl TaskRuntime {
    pub fn new() -> Self {
        let events = Channel::with_cap(DEFAULT_TASK_RUNTIME_EVENT_CHANNEL_COUNT);
        Self {
            events,
            parent: None,
            executors: HashMap::new(),
        }
    }
    pub fn generate_executor_key(&self, task_type: &TaskType, channel: &str) -> String {
        format!("{}-{}", task_type, channel)
    }
    pub fn raw_register_executor(
        &mut self,
        task_type: TaskType,
        executor: Arc<dyn TaskExecutor + Send + 'static>,
    ) {
        let channel = executor.channel();
        self.executors
            .insert(self.generate_executor_key(&task_type, &channel), executor);
    }
    pub fn register_executor<T: TaskExecutor + Send + 'static>(
        &mut self,
        task_type: TaskType,
        executor: T,
    ) -> &mut Self {
        self.raw_register_executor(task_type, Arc::new(executor));
        self
    }
    pub fn register_executor_ext<T, In, Out>(
        &mut self,
        task_type: TaskType,
        task_exec_ext: T,
    ) -> &mut Self
    where
        T: TaskExecutorExt<In, Out> + Send + 'static,
        In: Send + Sync + 'static,
        Out: Any + Send + Sync,
    {
        let executor = TaskExecutorExtImpl::new(task_exec_ext);
        self.register_executor(task_type, executor)
    }
    pub fn get_executor(
        &self,
        task_type: &TaskType,
        channel: &str,
    ) -> Option<Arc<dyn TaskExecutor + Send>> {
        self.executors
            .get(&self.generate_executor_key(task_type, channel))
            .cloned()
    }
    pub async fn exec(executor: Arc<dyn TaskExecutor + Send>, task: Task) -> TaskResult {
        let task_id = task.id.clone();
        let agent_id = task.agent_id.clone();
        match executor.execute(task).await {
            Ok(result) => result,
            Err(err) => TaskResult::new(
                fae_agent::error::TASK_ERROR_CODE_UNKNOWN,
                format!("{:?}", err),
                task_id,
                agent_id,
            ),
        }
    }
    pub fn into_self(&mut self) -> Self {
        let events = std::mem::replace(&mut self.events, Channel::with_cap(1));
        let parent = std::mem::replace(&mut self.parent, None);
        let executors = std::mem::take(&mut self.executors);
        Self {
            events,
            parent,
            executors,
        }
    }
}

impl Default for TaskRuntime {
    fn default() -> Self {
        Self::new().register_executor(TaskType::Model, ModelOpenAIApiExecutor::default())
            .register_executor_ext(TaskType::Tool, ToolExecutor::default())
            .into_self()
    }
}

#[async_trait::async_trait]
impl Environment for TaskRuntime {
    fn id(&self) -> &'static str {
        DEFAULT_TASK_RUNTIME_ID
    }

    async fn register_parent_env(&mut self, env: Env) {
        self.parent = Some(env);
    }

    async fn watch(&self) -> anyhow::Result<EnvEvent> {
        let self_fut = self.events.recv();
        if let Some(ref p) = self.parent {
            let parent_fut = p.watch();
            let event = tokio::select! {
                e = self_fut => e?,
                e = parent_fut => e?,
            };
            Ok(event)
        } else {
            let e = self_fut.await?;
            Ok(e)
        }
    }

    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>> {
        //为空时，查询所有任务执行器
        if select.select.is_none() {
            let items = self
                .executors
                .iter()
                .map(|(_, e)| ThingItem::Executor(e.desc()))
                .collect();
            let thing = Thing::new(self.id().to_string())
                .set_items(items)
                .into_self();
            return Ok(vec![thing]);
        }
        //根据任务类型和渠道查询
        if let ThingSelect::Executor(ref task_type, ref channel) = select.select {
            if let Some(e) = self
                .executors
                .get(&self.generate_executor_key(&task_type, channel))
            {
                return Ok(vec![
                    Thing::new(self.id().to_string())
                        .add_item(ThingItem::Executor(e.desc()))
                        .into_self(),
                ]);
            }
        }
        //根据工具名称查询
        if let ThingSelect::Tool(ref channel, ref _tool_name) = select.select {
            if let Some(e) = self
                .executors
                .get(&self.generate_executor_key(&TaskType::Tool, channel))
            {
                return e.query(select).await;
            }
        }
        //查询skill
        if let ThingSelect::Skill(ref channel, ref _name, ref _dir) = select.select {
            if let Some(e) = self
                .executors
                .get(&self.generate_executor_key(&TaskType::Skill, channel))
            {
                return e.query(select).await;
            }
        }
        //如果父环境也没有，就返回空
        if let Some(e) = self.parent.as_ref() {
            return e.query(select).await;
        }
        Ok(vec![])
    }

    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()> {
        //先检查
        for i in &tasks {
            let list = self
                .query(ThingSelect::Executor(
                    i.get_type().clone(),
                    i.get_exec_channel().to_string(),
                ).into())
                .await?;
            if list.is_empty() {
                return anyhow::anyhow!(
                    "[TaskRuntime:spawn]task executor not found: {:?}",
                    i.r#type
                )
                .err();
            }
        }
        //后执行
        for task in tasks {
            if let Some(e) = self.get_executor(task.get_type(), task.get_exec_channel()) {
                //优先自己执行
                let events_channel = self.events.clone();
                let executor = e.clone();
                tokio::spawn(async move {
                    let result = Self::exec(executor, task).await;
                    if let Err(e) = events_channel.send(EnvEvent::TaskResult(result)).await {
                        wd_log::log_error_ln!(
                            "[TaskRuntime:spawn] send task result error: {:?}",
                            e
                        );
                    };
                });
            } else if let Some(e) = self.parent.as_ref() {
                //再委托给父环境
                e.spawn(vec![task]).await?;
            } else {
                //如果父环境也没有，就报错
                return anyhow::anyhow!(
                    "[TaskRuntime:spawn] task executor not found: {:?}",
                    task.r#type
                )
                .err();
            }
        }
        Ok(())
    }

    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult> {
        if let Some(e) = self.get_executor(task.get_type(), task.get_exec_channel()) {
            //优先自己执行
            let result = Self::exec(e.clone(), task).await;
            Ok(result)
        } else if let Some(e) = self.parent.as_ref() {
            //再委托给父环境
            e.execute(task).await
        } else {
            //如果父环境也没有，就报错
            return anyhow::anyhow!(
                "[TaskRuntime:execute] task executor not found: {:?}",
                task.r#type
            )
            .err();
        }
    }
}

// TaskRuntimeRef is a reference to TaskRuntime.
#[derive(Clone)]
pub struct TaskRuntimeRef(Env);
impl TaskRuntimeRef {
    pub fn as_env(&self) -> Env {
        self.0.clone()
    }
}
impl From<TaskRuntime> for TaskRuntimeRef {
    fn from(runtime: TaskRuntime) -> Self {
        Self(Env::from(
            Arc::new(runtime) as Arc<dyn Environment + Send + 'static>
        ))
    }
}
impl From<Env> for TaskRuntimeRef {
    fn from(env: Env) -> Self {
        Self(env)
    }
}
impl Deref for TaskRuntimeRef {
    type Target = Env;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
