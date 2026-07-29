use std::{any::Any, sync::Arc, fmt::Debug};


#[async_trait::async_trait]
pub trait Context: Debug + Send + Sync + 'static {
    async fn get(&self, key: &str) -> Option<Box<dyn Any + Send + Sync + 'static>>;
    async fn set(&self, key: &str, value: Box<dyn Any + Send + Sync + 'static>) -> anyhow::Result<()>;
}


#[derive(Debug)]
pub struct Ctx(Arc<dyn Context>);