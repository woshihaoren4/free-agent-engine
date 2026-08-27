use std::collections::HashMap;
use std::ops::Deref;
use std::{fmt::Debug, sync::Arc};

use crate::{RT, common::AnyType};

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
    fn get_rt(&self) -> RT;
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

    fn get_rt(&self) -> RT {
        RT::null()
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
