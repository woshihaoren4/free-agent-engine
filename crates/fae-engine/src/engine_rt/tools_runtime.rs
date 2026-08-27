use fae_agent::Ctx;
use serde_json::Value;



#[async_trait::async_trait]
pub trait Tools {
    async fn load(&self,ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value>;
    async fn exec(&self,ctx: &Ctx, tool_name: &str, args: &Value) -> anyhow::Result<Value>;
}