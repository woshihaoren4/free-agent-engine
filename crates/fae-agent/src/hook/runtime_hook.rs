use std::sync::Arc;

use crate::Runtime;
pub use crate::RuntimeSelectExec as RuntimeHook;

#[async_trait::async_trait]
pub trait RuntimeHookBuilder: Send + Sync + 'static {
    async fn build(&self, rt: Vec<Arc<dyn Runtime>>) -> Arc<dyn Runtime>;
}
