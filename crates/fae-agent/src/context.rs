use std::{any::Any, sync::Arc, fmt::Debug};
use std::ops::Deref;
use crate::RT;

#[derive(Debug)]
pub struct ContextStack{
    pub use_time_mill: u64,
}

#[async_trait::async_trait]
pub trait Context: Debug + Send + Sync + 'static {
    fn append_stack(&self, key: &str, value:&str);
    fn get_rt(&self) -> RT;
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
