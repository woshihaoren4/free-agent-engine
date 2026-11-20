use super::*;
use crate::{
    hook::{SingleAgentHook, SingleAgentHookBuilder, SingleAgentHookContext},
    *,
};
use async_openai::types::chat::CreateChatCompletionRequest;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PluginAdapter {
    pub manager: Arc<PluginManager>,
}
impl PluginAdapter {
    pub fn new(manager: Arc<PluginManager>) -> Self {
        Self { manager }
    }
    fn snapshot(&self, ctx: &Ctx) -> Arc<PluginSnapshot> {
        ctx.plugin_run()
            .map(|r| r.snapshot.clone())
            .unwrap_or_else(|| self.manager.snapshot())
    }
    async fn invoke(
        &self,
        ctx: &Ctx,
        point: &str,
        method: &str,
        location: Option<&str>,
        metadata: Value,
        mut value: PluginValue,
    ) -> anyhow::Result<PluginValue> {
        let run = ctx.plugin_run();
        let root = run
            .as_ref()
            .and_then(|r| r.root.upgrade())
            .unwrap_or_else(|| ctx.clone());
        for node in self.snapshot(ctx).matching(point, location) {
            value = node
                .executor
                .invoke(ctx, &root, point, method, location, metadata.clone(), value)
                .await?;
        }
        Ok(value)
    }
    async fn agent<T: Serialize + DeserializeOwned + Send>(
        &self,
        ctx: &SingleAgentHookContext<'_>,
        method: &str,
        value: T,
    ) -> anyhow::Result<T> {
        let result = self
            .invoke(
                ctx.ctx,
                "SingleAgentHook",
                method,
                Some(&ctx.agent.name),
                metadata(ctx),
                PluginValue::Json(serde_json::to_value(value)?),
            )
            .await?;
        match result {
            PluginValue::Json(v) => Ok(serde_json::from_value(v)?),
            _ => anyhow::bail!("invalid plugin result"),
        }
    }
    async fn select_model(
        &self,
        ctx: &Ctx,
        model: &str,
    ) -> anyhow::Result<Option<(Arc<manager::PluginNode>, Vec<String>)>> {
        let run = ctx.plugin_run();
        let root = run
            .as_ref()
            .and_then(|r| r.root.upgrade())
            .unwrap_or_else(|| ctx.clone());
        let mut selected = None;
        for node in self.snapshot(ctx).matching("ModelHook", None) {
            let PluginValue::Json(value) = node
                .executor
                .invoke(
                    ctx,
                    &root,
                    "ModelHook",
                    "match_model",
                    None,
                    Value::Null,
                    PluginValue::Json(model.into()),
                )
                .await?
            else {
                anyhow::bail!("invalid match_model response");
            };
            let members: Vec<String> = serde_json::from_value(value)?;
            if !members.is_empty() {
                anyhow::ensure!(selected.is_none(), "plugin_model_conflict: {model}");
                selected = Some((node, members));
            }
        }
        Ok(selected)
    }
    pub async fn try_model(
        &self,
        ctx: &Ctx,
        request: CreateChatCompletionRequest,
    ) -> anyhow::Result<Option<ModelResponse>> {
        let Some((node, members)) = self.select_model(ctx, &request.model).await? else {
            return Ok(None);
        };
        let run = ctx.plugin_run();
        let root = run
            .as_ref()
            .and_then(|r| r.root.upgrade())
            .unwrap_or_else(|| ctx.clone());
        match node
            .executor
            .invoke(
                ctx,
                &root,
                "ModelHook",
                "exec",
                None,
                json!({"model_members":members}),
                PluginValue::Json(serde_json::to_value(request)?),
            )
            .await?
        {
            PluginValue::Model(response) => Ok(Some(response)),
            _ => anyhow::bail!("invalid plugin model response"),
        }
    }
    pub async fn model(
        &self,
        ctx: &Ctx,
        request: CreateChatCompletionRequest,
    ) -> anyhow::Result<ModelResponse> {
        let model = request.model.clone();
        self.try_model(ctx, request)
            .await?
            .ok_or_else(|| anyhow::anyhow!("plugin_model_not_found: {model}"))
    }
    pub fn tool(&self, ctx: &Ctx, name: &str) -> Option<Arc<dyn Tools>> {
        let channel = name.split_once("__").map(|(c, _)| c).unwrap_or(name);
        if self
            .snapshot(ctx)
            .matching("ToolsHook", Some(channel))
            .is_empty()
        {
            return None;
        }
        Some(Arc::new(PluginTools {
            adapter: self.clone(),
            channel: channel.into(),
        }))
    }
}
fn metadata(ctx: &SingleAgentHookContext<'_>) -> Value {
    json!({"plan_id":ctx.plan_id,"turn_id":ctx.turn_id,"task_id":ctx.task_id})
}
#[async_trait::async_trait]
impl SingleAgentHookBuilder for PluginAdapter {
    async fn build(&self) -> Arc<dyn SingleAgentHook> {
        Arc::new(self.clone())
    }
}
#[async_trait::async_trait]
impl crate::hook::ModelHook for PluginAdapter {
    async fn match_model(&self, ctx: &Ctx, model: &str) -> anyhow::Result<bool> {
        Ok(self.select_model(ctx, model).await?.is_some())
    }
    async fn exec(
        &self,
        ctx: &Ctx,
        request: CreateChatCompletionRequest,
    ) -> anyhow::Result<ModelResponse> {
        self.model(ctx, request).await
    }
}
#[async_trait::async_trait]
impl SingleAgentHook for PluginAdapter {
    async fn on_query(&self, c: &SingleAgentHookContext<'_>, v: String) -> anyhow::Result<String> {
        self.agent(c, "on_query", v).await
    }
    async fn on_prompt(&self, c: &SingleAgentHookContext<'_>, v: String) -> anyhow::Result<String> {
        self.agent(c, "on_prompt", v).await
    }
    async fn on_memory(
        &self,
        c: &SingleAgentHookContext<'_>,
        v: Vec<UserMemory>,
    ) -> anyhow::Result<Vec<UserMemory>> {
        self.agent(c, "on_memory", v).await
    }
    async fn on_history(
        &self,
        c: &SingleAgentHookContext<'_>,
        v: Vec<SessionMessage>,
    ) -> anyhow::Result<Vec<SessionMessage>> {
        self.agent(c, "on_history", v).await
    }
    async fn on_compression(
        &self,
        c: &SingleAgentHookContext<'_>,
        v: CreateChatCompletionRequest,
    ) -> anyhow::Result<CreateChatCompletionRequest> {
        self.agent(c, "on_compression", v).await
    }
    async fn on_model_req(
        &self,
        c: &SingleAgentHookContext<'_>,
        v: CreateChatCompletionRequest,
    ) -> anyhow::Result<CreateChatCompletionRequest> {
        self.agent(c, "on_model_req", v).await
    }
    async fn on_tools_req(
        &self,
        c: &SingleAgentHookContext<'_>,
        mut v: ToolRequest,
    ) -> anyhow::Result<ToolRequest> {
        let invocation = v.take_invocation();
        let result: ToolRequest = self.agent(c, "on_tools_req", v).await?;
        Ok(if let Some(i) = invocation {
            result.with_invocation(i)
        } else {
            result
        })
    }
    async fn on_tools_resp(
        &self,
        c: &SingleAgentHookContext<'_>,
        v: ToolResponse,
    ) -> anyhow::Result<ToolResponse> {
        match self
            .invoke(
                c.ctx,
                "SingleAgentHook",
                "on_tools_resp",
                Some(&c.agent.name),
                metadata(c),
                PluginValue::Tool(v),
            )
            .await?
        {
            PluginValue::Tool(v) => Ok(v),
            _ => anyhow::bail!("invalid plugin tool response"),
        }
    }
    async fn on_model_resp(
        &self,
        c: &SingleAgentHookContext<'_>,
        v: ModelResponse,
    ) -> anyhow::Result<ModelResponse> {
        match self
            .invoke(
                c.ctx,
                "SingleAgentHook",
                "on_model_resp",
                Some(&c.agent.name),
                metadata(c),
                PluginValue::Model(v),
            )
            .await?
        {
            PluginValue::Model(v) => Ok(v),
            _ => anyhow::bail!("invalid plugin model response"),
        }
    }
    async fn on_save(
        &self,
        c: &SingleAgentHookContext<'_>,
        v: Vec<SessionMessage>,
    ) -> anyhow::Result<Vec<SessionMessage>> {
        self.agent(c, "on_save", v).await
    }
}
#[derive(Debug)]
struct PluginTools {
    adapter: PluginAdapter,
    channel: String,
}
#[async_trait::async_trait]
impl Tools for PluginTools {
    fn channel(&self) -> &str {
        &self.channel
    }
    async fn desc(&self, ctx: &Ctx, name: &str) -> anyhow::Result<Value> {
        match self
            .adapter
            .invoke(
                ctx,
                "ToolsHook",
                "desc",
                Some(&self.channel),
                Value::Null,
                PluginValue::Json(name.into()),
            )
            .await?
        {
            PluginValue::Json(v) => {
                anyhow::ensure!(v.is_object(), "invalid tool description");
                Ok(v)
            }
            _ => anyhow::bail!("invalid tool description"),
        }
    }
    async fn exec(&self, ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        match self
            .adapter
            .invoke(
                ctx,
                "ToolsHook",
                "exec",
                Some(&self.channel),
                Value::Null,
                PluginValue::Json(serde_json::to_value(req)?),
            )
            .await?
        {
            PluginValue::Tool(v) => Ok(v),
            _ => anyhow::bail!("invalid tool response"),
        }
    }
}
