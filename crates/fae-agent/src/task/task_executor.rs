use crate::{Context, Select, Task, TaskReq, TaskResult, Thing};
use std::any::{Any};
use std::fmt::Debug;
use std::marker::PhantomData;

#[async_trait::async_trait]
pub trait Executor: Debug + Sync {
    fn desc(&self) -> String;
    fn channel(&self) -> String {
        "default".to_string()
    }
    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult>;
    async fn query(&self, _select: Select) -> anyhow::Result<Vec<Thing>> {
        Ok(vec![])
    }
}

#[async_trait::async_trait]
pub trait TaskExecutorExt<Out>: Debug + Sync {
    fn desc(&self) -> String;
    fn channel(&self) -> String {
        "default".to_string()
    }
    async fn exec(
        &self,
        ctx: Context,
        task_id: String,
        agent_id: String,
        user_id: String,
        input: TaskReq,
        ext: Option<Box<dyn Any + Send + Sync + 'static>>,
    ) -> anyhow::Result<Out>;
    async fn query(&self, _select: Select) -> anyhow::Result<Vec<Thing>> {
        Ok(vec![])
    }
}

#[derive(Debug)]
pub struct TaskExecutorExtImpl<T,  Out> {
    executor: T,
    _out: PhantomData<Out>,
}

impl<T,  Out> TaskExecutorExtImpl<T, Out> {
    pub fn new(executor: T) -> Self {
        Self {
            executor,
            _out: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<T, Out> Executor for TaskExecutorExtImpl<T, Out>
where
    T: TaskExecutorExt<Out>,
    Out: Debug + Any + Send + Sync,
{
    fn desc(&self) -> String {
        self.executor.desc()
    }

    fn channel(&self) -> String {
        self.executor.channel()
    }
    async fn execute(&self, mut task: Task) -> anyhow::Result<TaskResult> {
        let output = self
            .executor
            .exec(
                task.get_context(),
                task.id.clone(),
                task.agent_id.clone(),
                task.user_id,
                task.req,
                task.ext,
            )
            .await?;
        if (&output as &dyn Any).downcast_ref::<TaskResult>().is_some() {
            let task_result = (Box::new(output) as Box<dyn Any>)
                .downcast::<TaskResult>()
                .unwrap();
            return Ok(*task_result);
        }
        Ok(TaskResult::success(task.id.clone(), task.agent_id.clone()).set_data(output))
    }
    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>> {
        self.executor.query(select).await
    }
}
