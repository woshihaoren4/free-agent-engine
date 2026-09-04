use std::collections::HashMap;
use std::ops::Deref;
use std::{fmt::Debug, sync::Arc};

use crate::{Plan, RT, common::AnyType};

#[async_trait::async_trait]
pub trait Engine: Debug + Send + Sync + 'static {
    fn rt(&self) -> RT;
    async fn call(&self, ctx: Ctx, ty: String, env: AnyType) -> anyhow::Result<Box<dyn Plan>>;
}

pub type EngineRef = Arc<dyn Engine>;

#[derive(Debug)]
struct EngineNull;

#[async_trait::async_trait]
impl Engine for EngineNull {
    fn rt(&self) -> RT {
        RT::null()
    }

    async fn call(&self, _ctx: Ctx, _ty: String, _env: AnyType) -> anyhow::Result<Box<dyn Plan>> {
        anyhow::bail!("null engine cannot build plans")
    }
}

#[derive(Debug)]
pub struct ContextStack {
    pub use_time_mill: u64,
}

#[async_trait::async_trait]
pub trait Context: Debug + Send + Sync + 'static {
    fn append_stack(&self, _key: &str, _value: String) {}
    fn stacks(&self) -> HashMap<String, Vec<String>> {
        HashMap::new()
    }
    fn get_engine(&self) -> EngineRef;
    fn abort(&self) {}
    fn is_aborted(&self) -> bool {
        false
    }
    fn over(&self, _value: AnyType) {}
    fn error(&self, _error: String) {}
    fn is_completed(&self) -> bool {
        false
    }
    async fn wait(&self) -> anyhow::Result<AnyType> {
        anyhow::bail!("context does not support waiting")
    }
}

#[derive(Debug)]
pub struct ContextNull;
#[async_trait::async_trait]
impl Context for ContextNull {
    fn stacks(&self) -> HashMap<String, Vec<String>> {
        HashMap::new()
    }

    fn get_engine(&self) -> EngineRef {
        Arc::new(EngineNull)
    }
}

#[derive(Debug, Clone)]
pub struct Ctx(Arc<dyn Context>);
impl Ctx {
    pub fn new(ctx: Arc<dyn Context>) -> Self {
        Self(ctx)
    }

    pub(crate) fn null() -> Self {
        Self(Arc::new(ContextNull))
    }

    pub async fn result<T>(&self) -> anyhow::Result<T>
    where
        T: Send + 'static,
    {
        self.wait()
            .await?
            .downcast::<T>()
            .map(|value| *value)
            .map_err(|_| {
                anyhow::anyhow!(
                    "context result type does not match `{}`",
                    std::any::type_name::<T>()
                )
            })
    }
}

impl Deref for Ctx {
    type Target = dyn Context;

    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}
