use std::{any::Any, sync::Arc, fmt::Debug};
use std::ops::Deref;
use crate::RT;

#[derive(Debug)]
pub struct ContextStack{
    pub use_time_mill: u64,
}

#[async_trait::async_trait]
pub trait Context: Debug + Send + Sync + 'static {
    fn append_stack(&self, _key: &str, _value:String){
    }
    fn get_rt(&self) -> RT;
}

#[derive(Debug)]
pub struct ContextNull;
#[async_trait::async_trait]
impl Context for ContextNull{
    fn get_rt(&self) -> RT{
        RT::null()
    }
}



#[derive(Debug,Clone)]
pub struct Ctx(Arc<dyn Context>);
impl Ctx {
    pub fn new(ctx: Arc<dyn Context>) -> Self {
        Self(ctx)
    }
    pub(crate) fn null() -> Self {
        Self(Arc::new(ContextNull))
    }
}
impl Deref for Ctx {
    type Target = dyn Context;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}