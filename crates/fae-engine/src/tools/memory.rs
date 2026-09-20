use fae_agent::{
    Ctx, TaskMeta, TaskReq, TaskType, ToolInvocation, ToolRequest, ToolResponse, Tools,
    UserMemoryCategory, UserMemoryConfidence, UserMemoryRequest, UserMemoryResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    DEFAULT_CHANNEL, MEMORY_UPDATE, effective_tool_name, ok_json, parse_arguments,
    request_tool_name, unsupported_tool,
};

#[derive(Debug, Default)]
pub struct MemoryUpdateTool;

#[derive(Debug, Deserialize)]
struct MemoryUpdateArgs {
    #[serde(default)]
    id: Option<u64>,
    category: UserMemoryCategory,
    content: String,
    confidence: UserMemoryConfidence,
}

#[async_trait::async_trait]
impl Tools for MemoryUpdateTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != MEMORY_UPDATE {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": MEMORY_UPDATE,
            "description": "Create or update one durable memory for the current user. Omit id to create a memory; provide an existing id to replace it.",
            "parameters": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Existing memory ID to update. Omit when creating."
                    },
                    "category": {
                        "type": "string",
                        "enum": ["user_attribute", "preference", "other"],
                        "description": "The explicit category of this memory."
                    },
                    "content": {
                        "type": "string",
                        "description": "The concrete fact or preference to remember."
                    },
                    "confidence": {
                        "type": "string",
                        "enum": ["user_stated", "user_confirmed", "system_inferred"],
                        "description": "How the information was established."
                    }
                },
                "required": ["category", "content", "confidence"],
                "additionalProperties": false
            }
        }))
    }

    async fn exec(&self, ctx: &Ctx, mut req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != MEMORY_UPDATE {
            return Err(unsupported_tool(req.get_tool_name()));
        }
        let args: MemoryUpdateArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(response) => return Ok(response),
        };
        if args.content.trim().is_empty() {
            return Ok(ToolResponse::with_error(
                400,
                "content cannot be empty".to_string(),
            ));
        }
        let Some(ToolInvocation::UserMemory { user_id }) = req.take_invocation() else {
            return Ok(ToolResponse::with_error(
                400,
                "memory_update requires a single-agent user context".to_string(),
            ));
        };

        let response = ctx
            .get_engine()
            .rt()
            .exec::<_, UserMemoryResponse>(TaskReq {
                ctx: ctx.clone(),
                meta: TaskMeta {
                    ty: TaskType::Memory,
                    ..Default::default()
                },
                req: UserMemoryRequest::Update {
                    user_id,
                    id: args.id,
                    category: args.category,
                    content: args.content,
                    confidence: args.confidence,
                },
            })
            .await?;
        ok_json(response.resp)
    }
}
