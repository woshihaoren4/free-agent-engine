use super::{
    DEFAULT_CHANNEL, TIMER, effective_tool_name, ok_json, request_tool_name, unsupported_tool,
};
use fae_agent::{
    Ctx, TaskMeta, TaskReq, TaskType, TimerRequest, TimerResponse, ToolRequest, ToolResponse, Tools,
};
use serde_json::{Value, json};

#[derive(Debug, Default)]
pub struct TimerTool;
#[async_trait::async_trait]
impl Tools for TimerTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }
    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != TIMER {
            return Err(unsupported_tool(tool_name));
        }
        Ok(json!({
            "name": TIMER,
            "description": "Manage persistent cron timers for this user and agent. Each firing starts a NEW chat session in the bound workspace. Service must be running. Reuse idempotency_key on retries; use a new key for a distinct timer. Create requires name, input, schedule, idempotency_key. Update requires id and expected_revision from query; delete requires id and optionally expected_revision. Query include_occurrences=true shows execution status and session IDs. Pausing does not reset accepted_count; deleting does not cancel running sessions. Timezone defaults to UTC; display the timezone and next_local to the user.",
            "parameters": {
                "type": "object", "additionalProperties": false,
                "required": ["operation"],
                "properties": {
                    "operation": {"type": "string", "enum": ["create","query","update","delete"]},
                    "idempotency_key": {"type":"string", "minLength":1, "maxLength":256},
                    "name": {"type":"string", "minLength":1, "maxLength":256},
                    "input": {"type":"string", "minLength":1, "maxLength":65536},
                    "id": {"type":"string"},
                    "expected_revision": {"type":"integer", "minimum":1},
                    "schedule": {
                        "type":"object", "additionalProperties": false, "required":["kind","cron"],
                        "properties": {
                            "kind": {"type":"string","enum":["once","repeat","loop"]},
                            "cron": {"type":"string","description":"5 fields: minute hour day month weekday; 6 includes seconds; 7 also includes year. cron 0.16 dialect."},
                            "timezone": {"type":"string","description":"IANA timezone, e.g. Asia/Shanghai; defaults to UTC."},
                            "max_fires": {"type":"integer","minimum":1,"description":"Required only for repeat; total accepted firings."}
                        }
                    },
                    "misfire_policy": {"type":"string","enum":["coalesce","skip"]},
                    "state": {"type":"string","enum":["active","paused","completed","deleted"],"description":"Query filter."},
                    "enabled": {"type":"boolean","description":"Update: false pauses, true resumes."},
                    "include_deleted": {"type":"boolean"},
                    "limit": {"type":"integer","minimum":1,"maximum":100},
                    "cursor": {"type":"string"},
                    "include_occurrences": {"type":"boolean"}
                }
            }
        }))
    }
    async fn exec(&self, ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != TIMER {
            return Err(unsupported_tool(req.get_tool_name()));
        }
        let request: TimerRequest = match serde_json::from_str(req.get_arguments()) {
            Ok(request) => request,
            Err(e) => return Ok(error("invalid_arguments", &e.to_string())),
        };
        if ctx.scope().is_err() {
            return Ok(error(
                "execution_binding_unavailable",
                "Context has no bound identity",
            ));
        }
        let result = ctx
            .get_engine()
            .rt()
            .exec::<_, TimerResponse>(TaskReq {
                ctx: ctx.clone(),
                meta: TaskMeta {
                    ty: TaskType::Any("timer".into()),
                    executor: "timer".into(),
                    ..Default::default()
                },
                req: request,
            })
            .await;
        match result {
            Ok(result) => ok_json(result.resp),
            Err(e) => {
                let message = format!("{e:#}");
                let code = [
                    "invalid_cron",
                    "invalid_timezone",
                    "no_future_occurrence",
                    "invalid_schedule",
                    "invalid_arguments",
                    "execution_binding_unavailable",
                    "timer_not_found",
                    "revision_conflict",
                    "idempotency_conflict",
                    "resource_exhausted",
                    "timer_completed",
                    "timer_store_corrupt",
                ]
                .into_iter()
                .find(|code| message.contains(code))
                .unwrap_or("timer_unavailable");
                Ok(error(code, &message))
            }
        }
    }
}
fn error(code: &str, message: &str) -> ToolResponse {
    ToolResponse::with_error(400, json!({"code":code,"message":message}).to_string())
}
