use std::{fmt::Debug, sync::Arc};

use async_openai::types::chat::CreateChatCompletionRequest;

use crate::{
    Ctx, ModelResponse, SessionMessage, SingleAgentInfo, ToolRequest, ToolResponse, UserMemory,
};

#[derive(Debug)]
pub struct SingleAgentHookContext<'a> {
    pub ctx: &'a Ctx,
    pub plan_id: &'a str,
    pub turn_id: u64,
    pub agent: &'a SingleAgentInfo,
    pub task_id: Option<&'a str>,
}

#[async_trait::async_trait]
pub trait SingleAgentHookBuilder: Send + Sync + 'static {
    async fn build(&self) -> Arc<dyn SingleAgentHook>;
}

#[derive(Debug, Default)]
pub(crate) struct CompositeSingleAgentHook {
    hooks: Vec<Arc<dyn SingleAgentHook>>,
}

impl CompositeSingleAgentHook {
    pub(crate) fn new(hooks: Vec<Arc<dyn SingleAgentHook>>) -> Self {
        Self { hooks }
    }
}

#[async_trait::async_trait]
pub trait SingleAgentHook: Debug + Send + Sync + 'static {
    async fn on_prompt(
        &self,
        _ctx: &SingleAgentHookContext<'_>,
        prompt: String,
    ) -> anyhow::Result<String> {
        Ok(prompt)
    }

    async fn on_memory(
        &self,
        _ctx: &SingleAgentHookContext<'_>,
        memories: Vec<UserMemory>,
    ) -> anyhow::Result<Vec<UserMemory>> {
        Ok(memories)
    }

    async fn on_history(
        &self,
        _ctx: &SingleAgentHookContext<'_>,
        history: Vec<SessionMessage>,
    ) -> anyhow::Result<Vec<SessionMessage>> {
        Ok(history)
    }

    async fn on_compression(
        &self,
        _ctx: &SingleAgentHookContext<'_>,
        request: CreateChatCompletionRequest,
    ) -> anyhow::Result<CreateChatCompletionRequest> {
        Ok(request)
    }

    async fn on_model_req(
        &self,
        _ctx: &SingleAgentHookContext<'_>,
        request: CreateChatCompletionRequest,
    ) -> anyhow::Result<CreateChatCompletionRequest> {
        Ok(request)
    }

    async fn on_model_resp(
        &self,
        _ctx: &SingleAgentHookContext<'_>,
        response: ModelResponse,
    ) -> anyhow::Result<ModelResponse> {
        Ok(response)
    }

    async fn on_tools_req(
        &self,
        _ctx: &SingleAgentHookContext<'_>,
        request: ToolRequest,
    ) -> anyhow::Result<ToolRequest> {
        Ok(request)
    }

    async fn on_tools_resp(
        &self,
        _ctx: &SingleAgentHookContext<'_>,
        response: ToolResponse,
    ) -> anyhow::Result<ToolResponse> {
        Ok(response)
    }

    async fn on_save(
        &self,
        _ctx: &SingleAgentHookContext<'_>,
        messages: Vec<SessionMessage>,
    ) -> anyhow::Result<Vec<SessionMessage>> {
        Ok(messages)
    }
}

#[async_trait::async_trait]
impl SingleAgentHook for CompositeSingleAgentHook {
    async fn on_prompt(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        mut prompt: String,
    ) -> anyhow::Result<String> {
        for hook in &self.hooks {
            prompt = hook.on_prompt(ctx, prompt).await?;
        }
        Ok(prompt)
    }

    async fn on_memory(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        mut memories: Vec<UserMemory>,
    ) -> anyhow::Result<Vec<UserMemory>> {
        for hook in &self.hooks {
            memories = hook.on_memory(ctx, memories).await?;
        }
        Ok(memories)
    }

    async fn on_history(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        mut history: Vec<SessionMessage>,
    ) -> anyhow::Result<Vec<SessionMessage>> {
        for hook in &self.hooks {
            history = hook.on_history(ctx, history).await?;
        }
        Ok(history)
    }

    async fn on_compression(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        mut request: CreateChatCompletionRequest,
    ) -> anyhow::Result<CreateChatCompletionRequest> {
        for hook in &self.hooks {
            request = hook.on_compression(ctx, request).await?;
        }
        Ok(request)
    }

    async fn on_model_req(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        mut request: CreateChatCompletionRequest,
    ) -> anyhow::Result<CreateChatCompletionRequest> {
        for hook in &self.hooks {
            request = hook.on_model_req(ctx, request).await?;
        }
        Ok(request)
    }

    async fn on_model_resp(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        mut response: ModelResponse,
    ) -> anyhow::Result<ModelResponse> {
        for hook in &self.hooks {
            response = hook.on_model_resp(ctx, response).await?;
        }
        Ok(response)
    }

    async fn on_tools_req(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        mut request: ToolRequest,
    ) -> anyhow::Result<ToolRequest> {
        for hook in &self.hooks {
            request = hook.on_tools_req(ctx, request).await?;
        }
        Ok(request)
    }

    async fn on_tools_resp(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        mut response: ToolResponse,
    ) -> anyhow::Result<ToolResponse> {
        for hook in &self.hooks {
            response = hook.on_tools_resp(ctx, response).await?;
        }
        Ok(response)
    }

    async fn on_save(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        mut messages: Vec<SessionMessage>,
    ) -> anyhow::Result<Vec<SessionMessage>> {
        for hook in &self.hooks {
            messages = hook.on_save(ctx, messages).await?;
        }
        Ok(messages)
    }
}
