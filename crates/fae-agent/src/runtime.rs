use std::fmt::Debug;
use std::ops::Deref;
use std::sync::Arc;
use crate::{Event, Task, TaskResult};

#[async_trait::async_trait]
pub trait Runtime: Debug + Send + Sync + 'static {
    async fn watch(&self) -> anyhow::Result<Event>;
    async fn select(&self, key: &str) -> anyhow::Result<String>;
    async fn spawn(&self, tasks:Task) -> anyhow::Result<()>;
    async fn exec(&self, task:Task) -> anyhow::Result<TaskResult>;
    async fn kill(&self, task_id: &str) -> anyhow::Result<()>;
    async fn exit(&self);
}

#[derive(Debug)]
pub struct RT(Arc<dyn Runtime>);
impl RT {
    pub fn new(env: Arc<dyn Runtime>) -> Self {
        Self(env)
    }
}
impl Deref for RT {
    type Target = dyn Runtime;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}