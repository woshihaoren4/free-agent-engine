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
#[serde(untagged)]
enum MemoryUpdateArgs {
    Operation(MemoryOperationArgs),
    Legacy(LegacyMemoryUpdateArgs),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum MemoryOperationArgs {
    Create {
        category: UserMemoryCategory,
        content: String,
        confidence: UserMemoryConfidence,
    },
    Update {
        id: u64,
        category: UserMemoryCategory,
        content: String,
        confidence: UserMemoryConfidence,
    },
    Delete {
        id: u64,
    },
    Query,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyMemoryUpdateArgs {
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
            "description": "Create, update, delete, or query durable memories for the current user.",
            "parameters": {
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["create", "update", "delete", "query"],
                        "description": "Operation to perform. create requires category, content, and confidence; update also requires id; delete requires id; query requires no other fields."
                    },
                    "id": memory_id_schema(),
                    "category": memory_category_schema(),
                    "content": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The concrete fact or preference to create or update."
                    },
                    "confidence": memory_confidence_schema()
                },
                "required": ["operation"],
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
                req: match memory_request(user_id, args) {
                    Ok(request) => request,
                    Err(response) => return Ok(response),
                },
            })
            .await?;
        ok_json(response.resp)
    }
}

fn memory_request(
    user_id: String,
    args: MemoryUpdateArgs,
) -> Result<UserMemoryRequest, ToolResponse> {
    match args {
        MemoryUpdateArgs::Operation(MemoryOperationArgs::Create {
            category,
            content,
            confidence,
        }) => update_request(user_id, None, category, content, confidence),
        MemoryUpdateArgs::Operation(MemoryOperationArgs::Update {
            id,
            category,
            content,
            confidence,
        }) => update_request(user_id, Some(id), category, content, confidence),
        MemoryUpdateArgs::Operation(MemoryOperationArgs::Delete { id }) => {
            if id == 0 {
                return Err(invalid_arguments("id must be positive"));
            }
            Ok(UserMemoryRequest::Delete { user_id, id })
        }
        MemoryUpdateArgs::Operation(MemoryOperationArgs::Query) => {
            Ok(UserMemoryRequest::Query { user_id })
        }
        MemoryUpdateArgs::Legacy(args) => update_request(
            user_id,
            args.id,
            args.category,
            args.content,
            args.confidence,
        ),
    }
}

fn update_request(
    user_id: String,
    id: Option<u64>,
    category: UserMemoryCategory,
    content: String,
    confidence: UserMemoryConfidence,
) -> Result<UserMemoryRequest, ToolResponse> {
    if id == Some(0) {
        return Err(invalid_arguments("id must be positive"));
    }
    if content.trim().is_empty() {
        return Err(invalid_arguments("content cannot be empty"));
    }
    Ok(UserMemoryRequest::Update {
        user_id,
        id,
        category,
        content,
        confidence,
    })
}

fn invalid_arguments(message: &str) -> ToolResponse {
    ToolResponse::with_error(400, message.to_string())
}

fn memory_id_schema() -> Value {
    json!({
        "type": "integer",
        "minimum": 1,
        "description": "Existing memory ID."
    })
}

fn memory_category_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["user_attribute", "preference", "other"],
        "description": "The explicit category of this memory."
    })
}

fn memory_confidence_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["user_stated", "user_confirmed", "system_inferred"],
        "description": "How the information was established."
    })
}
