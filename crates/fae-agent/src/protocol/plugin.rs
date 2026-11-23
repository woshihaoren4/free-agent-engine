//! Registered worker capabilities. Only authenticated worker channels dispatch these paths.
use super::{Content, SessionSummary};
use crate::{ExecutionId, InvocationId, RequestId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

impl PluginCapability {
    fn validate(&self) -> Result<(), super::ProtocolError> {
        if self.handle.is_empty()
            || self.handle.len() > 1024
            || self.execution_id.is_some() != self.invocation_id.is_some()
        {
            return Err(super::ProtocolError::InvalidField("plugin capability"));
        }
        Ok(())
    }
}

impl super::RequestPayload {
    pub(crate) fn validate_plugin(
        &self,
        session: Option<&SessionId>,
    ) -> Result<(), super::ProtocolError> {
        use super::{ProtocolError, RequestPayload::*};
        let access = match self {
            PluginContextGet(p)
            | PluginContextExport(p)
            | PluginHostEngine(p)
            | PluginHostSession(p) => Some(p),
            PluginContextFork(p) => Some(&p.access),
            PluginContextCancel(p) => Some(&p.access),
            PluginOutputSend(p) => Some(&p.access),
            PluginNext(p) => Some(&p.access),
            PluginContextStacks(p) => Some(&p.access),
            PluginStreamNext(p) | PluginStreamClose(p) => p.access.as_ref(),
            _ => None,
        };
        if let Some(access) = access {
            access.binding.validate()?;
            if access.member.is_empty()
                || access.binding.execution_id.is_some() != session.is_some()
            {
                return Err(ProtocolError::InvalidField("plugin execution reference"));
            }
        }
        match self {
            PluginWorkerHello(_) | PluginInspect(_) if session.is_some() => {
                return Err(ProtocolError::InvalidField(
                    "plugin initialization session_id",
                ));
            }
            PluginInvoke(invoke) => {
                let methods: &[&str] = match invoke.point.as_str() {
                    "SingleAgentHook" => &[
                        "on_query",
                        "on_prompt",
                        "on_memory",
                        "on_history",
                        "on_compression",
                        "on_model_req",
                        "on_model_resp",
                        "on_tools_req",
                        "on_tools_resp",
                        "on_save",
                    ],
                    "ModelHook" => &["match_model", "exec"],
                    "ToolsHook" => &["desc", "exec"],
                    _ => return Err(ProtocolError::InvalidField("plugin hook point")),
                };
                if !methods.contains(&invoke.method.as_str())
                    || invoke.contexts.is_empty()
                    || invoke.timeout_ms == 0
                    || invoke.timeout_ms > 120_000
                {
                    return Err(ProtocolError::InvalidField("plugin hook method/budget"));
                }
                for context in invoke.contexts.values() {
                    context.binding.validate()?;
                    if context.session_id.as_ref() != session
                        || context.binding.execution_id.is_some() != session.is_some()
                    {
                        return Err(ProtocolError::InvalidField("plugin invocation context"));
                    }
                }
                validate_hook_value(&invoke.point, &invoke.method, &invoke.argument, false)?;
            }
            PluginNext(next) => {
                validate_hook_value(&next.point, &next.method, &next.argument, false)?
            }
            _ => {}
        }
        Ok(())
    }
}

/// The method discriminator selects the native provider or persisted business schema.
/// `Value` is a lossless wire carrier, never an unrestricted hook contract.
pub fn validate_hook_value(
    point: &str,
    method: &str,
    value: &Value,
    output: bool,
) -> Result<(), super::ProtocolError> {
    use async_openai::types::chat::{CreateChatCompletionRequest, CreateChatCompletionResponse};
    fn decodes<T: serde::de::DeserializeOwned>(value: &Value) -> bool {
        serde_json::from_value::<T>(value.clone()).is_ok()
    }
    let valid = match (point, method) {
        ("SingleAgentHook", "on_query" | "on_prompt") => value.is_string(),
        ("SingleAgentHook", "on_memory") => decodes::<Vec<crate::UserMemory>>(value),
        ("SingleAgentHook", "on_history" | "on_save") => {
            decodes::<Vec<crate::SessionMessage>>(value)
        }
        ("SingleAgentHook", "on_compression" | "on_model_req") => {
            decodes::<CreateChatCompletionRequest>(value)
        }
        ("SingleAgentHook", "on_tools_req") => decodes::<crate::ToolRequest>(value),
        ("SingleAgentHook", "on_tools_resp") | ("ToolsHook", "exec")
            if output || method == "on_tools_resp" =>
        {
            decodes::<PluginToolValue>(value)
        }
        ("ToolsHook", "exec") => decodes::<crate::ToolRequest>(value),
        ("ToolsHook", "desc") => {
            if output {
                value.is_object()
            } else {
                value.is_string()
            }
        }
        ("ModelHook", "match_model") => {
            if output {
                decodes::<Vec<String>>(value)
            } else {
                value.is_string()
            }
        }
        ("ModelHook", "exec") if !output => decodes::<CreateChatCompletionRequest>(value),
        ("ModelHook", "exec") | ("SingleAgentHook", "on_model_resp") => {
            match serde_json::from_value::<PluginModelValue>(value.clone()) {
                Ok(PluginModelValue::Completed { response }) => {
                    decodes::<CreateChatCompletionResponse>(&response)
                }
                Ok(PluginModelValue::Streaming { stream_id }) => !stream_id.is_empty(),
                Ok(PluginModelValue::HostStream { stream_id }) => output && !stream_id.is_empty(),
                Err(_) => false,
            }
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(super::ProtocolError::InvalidField("plugin hook value"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginModelValue {
    Completed { response: Value },
    Streaming { stream_id: String },
    HostStream { stream_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginToolValue {
    Input(PluginToolInput),
    Result(PluginToolResult),
    Reference(PluginToolReference),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginToolInput {
    pub result: Option<String>,
    pub reference: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginToolResult {
    pub result: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginToolReference {
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginHello {
    pub api_version: String,
    pub instances: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInspect {
    pub id: String,
    pub source: String,
}

/// This is a registry reference, never a business resource or restored Context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCapability {
    pub handle: String,
    pub execution_id: Option<ExecutionId>,
    pub invocation_id: Option<InvocationId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginContextRef {
    pub binding: PluginCapability,
    pub session_id: Option<SessionId>,
    pub summary: Option<SessionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInvoke {
    pub point: String,
    pub method: String,
    pub location: Option<String>,
    pub contexts: HashMap<String, PluginContextRef>,
    pub argument: Value,
    pub group: String,
    pub timeout_ms: u64,
    pub model_members: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginAccess {
    pub call_id: RequestId,
    pub member: String,
    pub binding: PluginCapability,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginFork {
    pub access: PluginAccess,
    pub spec: PluginChildSpec,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginChildSpec {
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCancel {
    pub access: PluginAccess,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginOutput {
    pub access: PluginAccess,
    pub content: Content,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginOutputReceipt {
    pub output_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginStream {
    pub call_id: RequestId,
    pub stream_id: String,
    pub access: Option<PluginAccess>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRelease {
    pub call_id: RequestId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEngineRef {
    pub handle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginNext {
    pub access: PluginAccess,
    pub point: String,
    pub method: String,
    pub argument: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginStacks {
    pub access: PluginAccess,
    pub query: crate::ContextStackQuery,
}
