use crate::*;
use serde_json::Value;

pub fn session(ctx: &Ctx, handle: &str) -> anyhow::Result<Value> {
    let reference = context(ctx, handle);
    Ok(if reference.session_id.is_some() {
        serde_json::to_value(reference)?
    } else {
        Value::Null
    })
}

/// Capabilities refer to host-owned public facts instead of duplicating scope identity.
pub fn context(ctx: &Ctx, handle: &str) -> crate::protocol::PluginContextRef {
    let scope = ctx.scope().ok();
    let execution = scope.as_ref().and_then(|scope| scope.execution.as_ref());
    crate::protocol::PluginContextRef {
        binding: crate::protocol::PluginCapability {
            handle: handle.into(),
            execution_id: execution.map(|e| e.execution_id.clone()),
            invocation_id: execution.map(|e| e.invocation_id.clone()),
        },
        session_id: execution.map(|e| e.session.key().session_id),
        summary: execution.map(|e| e.session.read_summary()),
    }
}
