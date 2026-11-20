use async_openai::types::chat::CreateChatCompletionRequest;

use crate::{Ctx, ModelResponse};

/// A model implementation selected by a runtime predicate.
#[async_trait::async_trait]
pub trait ModelHook: std::fmt::Debug + Send + Sync + 'static {
    async fn match_model(&self, ctx: &Ctx, model: &str) -> anyhow::Result<bool>;

    async fn exec(
        &self,
        ctx: &Ctx,
        request: CreateChatCompletionRequest,
    ) -> anyhow::Result<ModelResponse>;
}
