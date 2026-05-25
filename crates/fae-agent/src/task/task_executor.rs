use crate::{Task, TaskResult};
use std::any::Any;
use std::marker::PhantomData;
use wd_tools::PFErr;

#[async_trait::async_trait]
pub trait TaskExecutor: Sync {
    fn desc(&self) -> String;
    fn channel(&self) -> String {
        "default".to_string()
    }
    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult>;
}

#[async_trait::async_trait]
pub trait TaskExecutorExt<In, Out>: Sync {
    fn desc(&self) -> String;
    fn channel(&self) -> String {
        "default".to_string()
    }
    async fn exec(
        &self,
        task_id: String,
        agent_id: String,
        user_id: String,
        input: In,
    ) -> anyhow::Result<Out>;
}

pub struct TaskExecutorExtImpl<T, In, Out> {
    executor: T,
    _in: PhantomData<In>,
    _out: PhantomData<Out>,
}

impl<T, In, Out> TaskExecutorExtImpl<T, In, Out> {
    pub fn new(executor: T) -> Self {
        Self {
            executor,
            _in: PhantomData,
            _out: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<T, In, Out> TaskExecutor for TaskExecutorExtImpl<T, In, Out>
where
    T: TaskExecutorExt<In, Out>,
    In: Send + Sync + 'static,
    Out: Any + Send + Sync,
{
    fn desc(&self) -> String {
        self.executor.desc()
    }

    fn channel(&self) -> String {
        self.executor.channel()
    }
    async fn execute(&self, mut task: Task) -> anyhow::Result<TaskResult> {
        let input = if let Some(s) = task.into_inner::<In>() {
            s
        } else {
            return Err(anyhow::anyhow!(
                "[TaskExecutorExtImpl] task input type error. expect: {:?}",
                task
            ))?;
        };
        let output = self
            .executor
            .exec(task.id.clone(), task.agent_id.clone(), task.user_id, input)
            .await?;
        if (&output as &dyn Any).downcast_ref::<TaskResult>().is_some() {
            let task_result = (Box::new(output) as Box<dyn Any>)
                .downcast::<TaskResult>()
                .unwrap();
            return Ok(*task_result);
        }
        Ok(TaskResult::success(task.id.clone(), task.agent_id.clone()))
    }
}
