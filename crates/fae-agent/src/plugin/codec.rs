use crate::*;
use serde_json::{Value, json};

pub fn scope(ctx: &Ctx, handle: &str) -> anyhow::Result<Value> {
    let s = ctx.scope()?;
    Ok(
        json!({"scope_id":s.scope_id,"parent_scope_id":s.parent_scope_id,
            "user":{"user_id":s.user.user_id},"session": session(ctx,handle)?,
            "agent_id":s.agent_id,"workflow_id":s.workflow_id,"node_id":s.node_id,"turn_id":s.turn_id,
            "todo":s.todo.as_ref().map(|t| json!({"handle":handle,"plan_id":t.plan_id,"agent_id":t.agent_id,
                "user_id":t.user_id,"user_input":t.user_input})),
            "workspace":s.workspace,"execution_origin":s.execution_origin,
            "timer_execution":s.timer_execution.as_ref().map(|t| json!({"owner":t.owner,"source":source(&t.source),"workspace":t.workspace}))
        }),
    )
}
pub fn source(source: &SingleAgentSource) -> Value {
    match source {
        SingleAgentSource::AgentId(id) => json!({"kind":"agent_id","agent_id":id}),
        SingleAgentSource::Paths { config, prompt } => {
            json!({"kind":"paths","config":config,"prompt":prompt})
        }
    }
}
pub fn source_from_json(value: &Value) -> anyhow::Result<SingleAgentSource> {
    let required = |key: &str| {
        value[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Invalid timer source"))
    };
    Ok(match value["kind"].as_str() {
        Some("agent_id") => SingleAgentSource::AgentId(required("agent_id")?.into()),
        Some("paths") => SingleAgentSource::Paths {
            config: required("config")?.into(),
            prompt: required("prompt")?.into(),
        },
        _ => anyhow::bail!("Invalid timer source"),
    })
}
pub fn session(ctx: &Ctx, handle: &str) -> anyhow::Result<Value> {
    if ctx.get_session()?.is_none() {
        return Ok(Value::Null);
    }
    let s = ctx.scope()?;
    let b = s
        .session
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("SessionUnavailable"))?;
    Ok(
        json!({"handle":handle,"identity":{"session_id":b.session_id,"user_id":s.user.user_id,
        "agent_id":s.agent_id,"turn_id":s.turn_id}}),
    )
}
pub fn context(ctx: &Ctx, handle: &str, root: &Ctx, metadata: &Value) -> Value {
    let s = ctx.scope().ok();
    let mut identity = json!({"scope_id":s.as_ref().map(|s|&s.scope_id),
        "agent_id":s.as_ref().and_then(|s|s.agent_id.as_ref()),
        "user_id":s.as_ref().map(|s|&s.user.user_id),"turn_id":s.as_ref().and_then(|s|s.turn_id),
        "run_id":root.plugin_run().map(|r| r.run_id.clone())});
    for key in ["plan_id", "task_id", "turn_id"] {
        if let Some(value) = metadata.get(key) {
            identity[key] = value.clone();
        }
    }
    json!({"handle":handle,"identity":identity,"session":session(ctx,handle).unwrap_or(Value::Null)})
}
pub fn state(state: &ContextState) -> Value {
    match state {
        ContextState::Created => json!({"kind":"created"}),
        ContextState::Running => json!({"kind":"running"}),
        ContextState::Failed { error } => {
            json!({"kind":"failed","error":{"message":error.message}})
        }
        ContextState::Cancelled { reason } => json!({"kind":"cancelled","reason":reason.as_ref()}),
        ContextState::Succeeded { output } => {
            json!({"kind":"succeeded","output": match output.json_snapshot() {
                Ok(value) => json!({"json":value}),
                Err(_) => json!({"handle":"unsupported-output"}),
            }})
        }
    }
}
pub fn phase(value: &Value) -> anyhow::Result<ContextStackPhase> {
    Ok(match value.as_str().unwrap_or("") {
        "dispatched" => ContextStackPhase::Dispatched,
        "started" => ContextStackPhase::Started,
        "succeeded" => ContextStackPhase::Succeeded,
        "failed" => ContextStackPhase::Failed,
        "cancelled" => ContextStackPhase::Cancelled,
        "cleanup_finished" => ContextStackPhase::CleanupFinished,
        _ => anyhow::bail!("Invalid ContextStackPhase"),
    })
}
pub fn meta(value: &Value) -> anyhow::Result<TaskMeta> {
    let mut v = value.clone();
    let kind = v["ty"]["kind"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid TaskType"))?;
    v["ty"] = if kind == "any" {
        json!({"Any":v["ty"]["value"]})
    } else {
        json!(match kind {
            "tool" => "Tool",
            "skill" => "Skill",
            "mcp" => "Mcp",
            "plan" => "Plan",
            "workflow" => "Workflow",
            "model" => "Model",
            "session" => "Session",
            "memory" => "Memory",
            _ => anyhow::bail!("Invalid TaskType"),
        })
    };
    Ok(serde_json::from_value(v)?)
}
pub fn stacks(entries: Vec<ContextStack>) -> anyhow::Result<Value> {
    let mut value = serde_json::to_value(entries)?;
    for entry in value.as_array_mut().unwrap() {
        entry["phase"] = json!(match entry["phase"].as_str().unwrap() {
            "CleanupFinished" => "cleanup_finished",
            s => match s {
                "Dispatched" => "dispatched",
                "Started" => "started",
                "Succeeded" => "succeeded",
                "Failed" => "failed",
                _ => "cancelled",
            },
        });
        let ty = entry["task_meta"]["ty"].clone();
        entry["task_meta"]["ty"] = if let Some(ty) = ty.as_str() {
            json!({"kind":ty.to_lowercase()})
        } else {
            json!({"kind":"any","value":ty["Any"]})
        };
    }
    Ok(value)
}
