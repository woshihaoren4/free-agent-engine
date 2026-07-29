use std::{any::Any, sync::Arc, fmt::Debug};
use std::ops::Deref;

#[async_trait::async_trait]
pub trait Context: Debug + Send + Sync + 'static {
    async fn get(&self, key: &str) -> Option<Box<dyn Any + Send + Sync + 'static>>;
    async fn set(&self, key: &str, value: Box<dyn Any + Send + Sync + 'static>) -> anyhow::Result<()>;
}


#[derive(Debug,Clone)]
pub struct Ctx(Arc<dyn Context>);
impl Ctx {
    pub fn new(ctx: Arc<dyn Context>) -> Self {
        Self(ctx)
    }
}
impl Deref for Ctx {
    type Target = dyn Context;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}
