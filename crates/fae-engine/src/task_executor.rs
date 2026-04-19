use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use wd_tools::channel::Channel;
use wd_tools::PFErr;
use fae_agent::{Env, EnvEvent, Environment, Task, TaskExecutor, TaskResult, TaskType, Thing, ThingSelect};

pub struct TaskRuntime {
    executors: HashMap<TaskType, Arc<dyn TaskExecutor+Send+'static>>,
}

impl TaskRuntime {
    pub fn new() -> Self {
        Self { executors: HashMap::new()}
    }
}

#[async_trait::async_trait]
impl Environment for TaskRuntime{
    fn id(&self) -> &'static str {
        "task_runtime"
    }

    async fn register_parent_env(&mut self, env: Env) {
        panic!("task_runtime not support register_parent_env");
    }

    async fn watch(&self) -> Channel<EnvEvent> {
        panic!("task_runtime not support watch");
    }

    async fn query(&self, select: ThingSelect) -> anyhow::Result<Vec<Thing>> {
        panic!("task_runtime not support query");
    }

    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()> {
        panic!("task_runtime not support spawn");
    }

    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult> {
        panic!("task_runtime not support execute");
    }
}


// TaskRuntimeRef is a reference to TaskRuntime.
#[derive(Clone)]
pub struct TaskRuntimeRef(Arc<TaskRuntime>);
impl From<TaskRuntime> for TaskRuntimeRef {
    fn from(runtime: TaskRuntime) -> Self {Self(Arc::new(runtime))}
}
impl Deref for TaskRuntimeRef {
    type Target = TaskRuntime;
    fn deref(&self) -> &Self::Target {&self.0}
}
