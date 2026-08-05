use std::fmt::Debug;
use std::ops::Deref;
use std::sync::Arc;
use crate::{Event, Task, TaskResult};

#[async_trait::async_trait]
pub trait Environment: Debug + Send + Sync + 'static {
    async fn watch(&self) -> anyhow::Result<Event>;
    async fn select(&self, key: &str) -> anyhow::Result<String>;
    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()>;
    async fn exec(&self, task:Task) -> anyhow::Result<TaskResult>;
    async fn kill(&self, tid: &str) -> anyhow::Result<()>;
    async fn exit(&self);
}

#[derive(Debug)]
pub struct Env(Arc<dyn Environment>);
impl Env {
    pub fn new(env: Arc<dyn Environment>) -> Self {
        Self(env)
    }
}
impl Deref for Env {
    type Target = dyn Environment;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}