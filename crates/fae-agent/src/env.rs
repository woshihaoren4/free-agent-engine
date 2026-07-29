use std::fmt::Debug;
use std::ops::Deref;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait Environment: Debug + Send + Sync + 'static {
    async fn watch(&self) -> anyhow::Result<String>;
    async fn select(&self, key: &str) -> anyhow::Result<String>;
    async fn spawn(&self, key: &str) -> anyhow::Result<String>;
    async fn exec(&self, key: &str) -> anyhow::Result<String>;
    async fn kill(&self, key: &str) -> anyhow::Result<()>;
    async fn exit(&self, key: &str) -> anyhow::Result<()>;
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