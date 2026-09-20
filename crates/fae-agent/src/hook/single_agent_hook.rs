use std::fmt::Debug;

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
