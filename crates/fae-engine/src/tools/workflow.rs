use fae_agent::{
    Ctx, TaskMeta, TaskReq, TaskType, ToolInvocation, ToolRequest, ToolResponse, Tools, WorkflowEnv,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    DEFAULT_CHANNEL, WORKFLOW, effective_tool_name, ok_json, parse_arguments, request_tool_name,
    unsupported_tool,
};

#[derive(Debug, Default)]
pub struct WorkflowTool;

#[derive(Debug, Deserialize)]
struct WorkflowArgs {
    workflow_id: String,
    #[serde(default)]
    input: Value,
}

#[async_trait::async_trait]
impl Tools for WorkflowTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != WORKFLOW {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": WORKFLOW,
            "description": "Run a configured workflow and return its final JSON output.",
            "parameters": {
                "type": "object",
                "properties": {
                    "workflow_id": {
                        "type": "string",
                        "description": "Workflow ID loaded from FAE_HOST/workflows."
                    },
                    "input": {
                        "description": "JSON input passed to the workflow."
                    }
                },
                "required": ["workflow_id"],
                "additionalProperties": false
            }
        }))
    }

    async fn exec(&self, ctx: &Ctx, mut req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != WORKFLOW {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: WorkflowArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };
        if args.workflow_id.trim().is_empty() {
            return Ok(ToolResponse::with_error(
                400,
                "workflow_id cannot be empty".to_string(),
            ));
        }

        let (env, _) = match req.take_invocation() {
            Some(ToolInvocation::Workflow { user_id }) => {
                WorkflowEnv::new_with_user_id(args.workflow_id, args.input, user_id)
            }
            Some(ToolInvocation::Agent(_)) | None => WorkflowEnv::new(args.workflow_id, args.input),
        };
        let response = ctx
            .get_engine()
            .rt()
            .exec::<_, Value>(TaskReq {
                ctx: ctx.clone(),
                meta: TaskMeta {
                    ty: TaskType::Workflow,
                    ..Default::default()
                },
                req: env,
            })
            .await?;
        ok_json(response.resp)
    }
}
