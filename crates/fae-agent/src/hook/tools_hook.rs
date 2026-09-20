use std::sync::Arc;

use crate::Tools;

pub use crate::Tools as ToolsHook;

#[async_trait::async_trait]
pub trait ToolsHookBuilder: Send + Sync + 'static {
    async fn build(&self, tools: Arc<dyn Tools>) -> Arc<dyn Tools>;
}
