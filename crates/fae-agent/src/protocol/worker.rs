//! Internal provider RPC envelopes, accepted only on authenticated worker channels.
use super::{PlanRef, PluginAccess, ProtocolApiVersion, ProtocolError, RuntimeCallRef, SessionRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerContextRef {
    pub context_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerTaskMeta {
    pub id: String,
    pub ty: crate::TaskType,
    pub executor: Option<String>,
    pub plan_run_id: Option<String>,
    pub publisher: Option<String>,
    pub origin_plan_id: Option<String>,
    pub target_plan_id: Option<String>,
}
impl From<&crate::TaskMeta> for WorkerTaskMeta {
    fn from(meta: &crate::TaskMeta) -> Self {
        fn optional(s: &str) -> Option<String> {
            (!s.is_empty()).then(|| s.to_owned())
        }
        Self {
            id: meta.id.clone(),
            ty: meta.ty.clone(),
            executor: optional(&meta.executor),
            plan_run_id: optional(&meta.plan_run_id),
            publisher: optional(&meta.publisher),
            origin_plan_id: meta.origin_plan_id.clone(),
            target_plan_id: meta.target_plan_id.clone(),
        }
    }
}
impl WorkerTaskMeta {
    pub fn into_meta(self) -> crate::TaskMeta {
        crate::TaskMeta {
            id: self.id,
            ty: self.ty,
            executor: self.executor.unwrap_or_default(),
            plan_run_id: self.plan_run_id.unwrap_or_default(),
            publisher: self.publisher.unwrap_or_default(),
            origin_plan_id: self.origin_plan_id,
            target_plan_id: self.target_plan_id,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerTask {
    pub meta: WorkerTaskMeta,
    pub context: Option<WorkerContextRef>,
    pub payload: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerPlanNext {
    Tasks { tasks: Vec<WorkerTask> },
    End,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<WorkerContextRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<WorkerTaskMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ty: Option<crate::TaskType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<crate::ContextStackQuery>,
    pub access: Option<PluginAccess>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRequestMessage {
    pub api_version: ProtocolApiVersion,
    pub request_id: crate::RequestId,
    pub event: String,
    pub payload: Value,
    pub session: Option<SessionRef>,
    pub plan: Option<PlanRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<RuntimeCallRef>,
    pub binding: WorkerBinding,
}
impl WorkerRequestMessage {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let (required, plan, runtime, null): (&[&str], bool, bool, bool) = match self.event.as_str()
        {
            "/request/plugin/plan/create" => (
                &["generation", "context", "instance_id", "access"],
                true,
                false,
                false,
            ),
            "/request/plugin/plan/init" | "/request/plugin/plan/release" => {
                (&["instance_id", "access"], true, false, true)
            }
            "/request/plugin/plan/next" => (&["instance_id", "access"], true, false, false),
            "/request/plugin/plan/abort" => (
                &["instance_id", "access", "code", "error"],
                true,
                false,
                true,
            ),
            "/request/plugin/runtime/exec" => (
                &["generation", "meta", "context", "access"],
                false,
                true,
                false,
            ),
            "/request/plugin/runtime/select" => (
                &["generation", "ty", "context", "access"],
                false,
                true,
                false,
            ),
            "/request/plugin/runtime/kill" => (
                &["generation", "context", "task_id", "access"],
                false,
                true,
                true,
            ),
            "/request/plugin/runtime/exit" => (&["generation", "access"], false, true, true),
            "/request/plugin/context/complete" => (&["access"], false, false, false),
            "/request/plugin/context/get" => (&["access", "context"], false, false, true),
            "/request/plugin/host/runtime/exec" => {
                (&["meta", "context", "access"], false, true, false)
            }
            "/request/plugin/host/runtime/select" => {
                (&["ty", "context", "access"], false, true, false)
            }
            "/request/plugin/host/plan/call" => (&["access"], true, false, false),
            "/request/plugin/host/stream/next" | "/request/plugin/host/stream/close" => {
                (&["access", "context"], false, false, false)
            }
            "/request/plugin/host/stream/register" => (&["access", "context"], false, false, false),
            "/request/plugin/resource/next" | "/request/plugin/resource/close" => {
                (&["generation", "access", "context"], false, false, false)
            }
            _ => return Err(ProtocolError::InvalidField("worker event")),
        };
        if self.plan.is_some() != plan
            || self.call.is_some() != runtime
            || (null && !self.payload.is_null())
        {
            return Err(ProtocolError::InvalidField("worker route/payload"));
        }
        let binding =
            serde_json::to_value(&self.binding).map_err(|_| ProtocolError::PayloadMismatch)?;
        let fields = binding
            .as_object()
            .ok_or(ProtocolError::InvalidField("binding"))?;
        if required
            .iter()
            .any(|key| fields.get(*key).is_none_or(Value::is_null))
            || fields.keys().any(|key| {
                !required.contains(&key.as_str())
                    && !(self.event == "/request/plugin/context/get" && key == "query")
                    && !(self.event == "/request/plugin/context/complete" && key == "resources")
            })
        {
            return Err(ProtocolError::InvalidField("binding fields"));
        }
        if let Some(plan) = &self.plan {
            crate::plugin::validate_definition_id(&plan.plan_id)
                .map_err(|_| ProtocolError::InvalidField("plan"))?;
        }
        if let Some(call) = &self.call {
            crate::plugin::validate_definition_id(&call.runtime_id)
                .map_err(|_| ProtocolError::InvalidField("call"))?;
            if let Some(meta) = &self.binding.meta {
                if meta.executor.as_deref() != Some(&call.runtime_id) || meta.id.is_empty() {
                    return Err(ProtocolError::InvalidField("meta executor/id"));
                }
            }
        }
        Ok(())
    }
    pub fn validate_response(
        &self,
        response: &super::ResponseMessage,
    ) -> Result<(), ProtocolError> {
        response.validate()?;
        if response.response_id != self.request_id
            || response.session != self.session
            || response.plan != self.plan
        {
            return Err(ProtocolError::PayloadMismatch);
        }
        if response.code == super::ResponseCode::Success {
            match self.event.as_str() {
                "/request/plugin/plan/create" if !response.payload.is_string() => {
                    return Err(ProtocolError::PayloadMismatch);
                }
                "/request/plugin/plan/init" | "/request/plugin/plan/next" => {
                    serde_json::from_value::<WorkerPlanNext>(response.payload.clone())
                        .map_err(|_| ProtocolError::PayloadMismatch)?;
                }
                "/request/plugin/plan/abort"
                | "/request/plugin/plan/release"
                | "/request/plugin/runtime/kill"
                | "/request/plugin/runtime/exit"
                | "/request/plugin/context/complete"
                    if !response.payload.is_null() =>
                {
                    return Err(ProtocolError::PayloadMismatch);
                }
                _ => {}
            }
        }
        Ok(())
    }
}
impl<'de> Deserialize<'de> for WorkerRequestMessage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            api_version: ProtocolApiVersion,
            request_id: crate::RequestId,
            event: String,
            payload: Value,
            session: Option<SessionRef>,
            plan: Option<PlanRef>,
            #[serde(default)]
            call: Option<RuntimeCallRef>,
            binding: WorkerBinding,
        }
        let value = Value::deserialize(d)?;
        for field in [
            "api_version",
            "request_id",
            "event",
            "payload",
            "session",
            "plan",
            "binding",
        ] {
            if value.get(field).is_none() {
                return Err(serde::de::Error::custom(format!("missing {field}")));
            }
        }
        let wire: Wire = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        let message = Self {
            api_version: wire.api_version,
            request_id: wire.request_id,
            event: wire.event,
            payload: wire.payload,
            session: wire.session,
            plan: wire.plan,
            call: wire.call,
            binding: wire.binding,
        };
        message.validate().map_err(serde::de::Error::custom)?;
        Ok(message)
    }
}
