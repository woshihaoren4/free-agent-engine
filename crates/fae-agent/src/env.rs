use std::fmt::Debug;

#[async_trait::async_trait]
pub trait Environment: Debug + Send + Sync + 'static {
    async fn watch(&self) -> anyhow::Result<String>;
    async fn select(&self, key: &str) -> anyhow::Result<String>;
    async fn spawn(&self, key: &str) -> anyhow::Result<String>;
    async fn exec(&self, key: &str) -> anyhow::Result<String>;
    async fn kill(&self, key: &str) -> anyhow::Result<()>;
    async fn exit(&self, key: &str) -> anyhow::Result<()>;
}

pub struct Env {
    
}
