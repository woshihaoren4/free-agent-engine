use fae_agent::{
    Ctx, SingleAgentEnv, TaskMeta, TaskReq, TaskType, ToolInvocation, ToolRequest, ToolResponse,
    Tools, to_plan_ty,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    AGENT, DEFAULT_CHANNEL, effective_tool_name, parse_arguments, request_tool_name,
    unsupported_tool,
};

#[derive(Debug, Default)]
pub struct AgentTool;

#[derive(Debug, Deserialize)]
struct AgentArgs {
    agent_id: String,
    input: String,
    session_id: Option<String>,
}

#[async_trait::async_trait]
impl Tools for AgentTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != AGENT {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": AGENT,
            "description": "Run a configured single agent and return its final output.",
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "Agent ID loaded from FAE_HOST/agents."
                    },
                    "input": {
                        "type": "string",
                        "description": "The complete task and context for the agent."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional session ID override."
                    }
                },
                "required": ["agent_id", "input"],
                "additionalProperties": false
            }
        }))
    }

    async fn exec(&self, ctx: &Ctx, mut req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != AGENT {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: AgentArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };
        if args.agent_id.trim().is_empty() {
            return Ok(ToolResponse::with_error(
                400,
                "agent_id cannot be empty".to_string(),
            ));
        }
        if args.input.trim().is_empty() {
            return Ok(ToolResponse::with_error(
                400,
                "input cannot be empty".to_string(),
            ));
        }

        let (mut env, session) = match req.take_invocation() {
            Some(ToolInvocation::Agent(invocation)) => {
                invocation.into_env(&args.agent_id, args.input)?
            }
            Some(ToolInvocation::Workflow { .. }) | None => {
                SingleAgentEnv::from_agent_id(args.agent_id, args.input)
            }
        };
        if let Some(session_id) = args.session_id {
            env = env.with_session_id(session_id);
        }

        let engine = ctx.get_engine();
        let plan = engine
            .call(ctx.clone(), to_plan_ty::<SingleAgentEnv>(), Box::new(env))
            .await?;
        engine
            .rt()
            .exec::<_, ()>(TaskReq {
                ctx: ctx.clone(),
                meta: TaskMeta {
                    ty: TaskType::Plan,
                    ..Default::default()
                },
                req: plan,
            })
            .await?;

        let output = session.result().await?;
        let output = match output {
            Value::String(output) => output,
            output => serde_json::to_string(&output)?,
        };
        Ok(ToolResponse::with_result(output))
    }
}
