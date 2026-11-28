//! Public requests carry raw business JSON and never worker capabilities.
use super::*;
use crate::PlanTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMessage {
    pub api_version: ProtocolApiVersion,
    pub request_id: crate::RequestId,
    pub event: RequestEvent,
    pub payload: Value,
    pub session: Option<SessionRef>,
    pub plan: Option<PlanRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<RuntimeCallRef>,
}
impl RequestMessage {
    pub fn into_message(self) -> Result<Message, ProtocolError> {
        if self.event.is_plugin() {
            return Err(ProtocolError::InvalidField("public event"));
        }
        if !matches!(
            self.event,
            RequestEvent::RuntimeExec | RequestEvent::RuntimeSelect
        ) && self.call.is_some()
        {
            return Err(ProtocolError::InvalidField("call"));
        }
        if !self.event.is_session() && self.event != RequestEvent::PlanCall && self.plan.is_some() {
            return Err(ProtocolError::InvalidField("plan"));
        }
        let payload = adapt_public_payload(
            MessageEvent::Request(self.event),
            self.plan.as_ref(),
            self.payload,
        )?;
        let payload = RequestPayload::decode(self.event, payload)
            .map_err(|_| ProtocolError::PayloadMismatch)?;
        let message = Message {
            api_version: self.api_version,
            request_id: Some(self.request_id),
            session_id: self.session.map(|s| s.session_id),
            stream_id: None,
            seq: None,
            event: MessageEvent::Request(self.event),
            payload: MessagePayload::Request(payload),
            plan: self.plan,
            call: self.call,
        };
        message.validate()?;
        Ok(message)
    }
}
impl<'de> Deserialize<'de> for RequestMessage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            api_version: ProtocolApiVersion,
            request_id: crate::RequestId,
            event: RequestEvent,
            payload: Value,
            session: Option<SessionRef>,
            plan: Option<PlanRef>,
            call: Option<RuntimeCallRef>,
        }
        let value = Value::deserialize(d)?;
        for key in [
            "api_version",
            "request_id",
            "event",
            "payload",
            "session",
            "plan",
        ] {
            if value.get(key).is_none() {
                return Err(serde::de::Error::custom(format!("missing {key}")));
            }
        }
        let wire: Wire = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        let request = Self {
            api_version: wire.api_version,
            request_id: wire.request_id,
            event: wire.event,
            payload: wire.payload,
            session: wire.session,
            plan: wire.plan,
            call: wire.call,
        };
        request
            .clone()
            .into_message()
            .map_err(serde::de::Error::custom)?;
        Ok(request)
    }
}

pub(crate) fn target_plan_ref(target: &PlanTarget) -> PlanRef {
    PlanRef {
        plan_id: match target {
            PlanTarget::Agent { .. } => "single_agent".into(),
            PlanTarget::Workflow { .. } => "workflow".into(),
            PlanTarget::PluginPlan { plan_id, .. } => plan_id.clone(),
        },
    }
}
fn target_payload(target: &PlanTarget) -> Value {
    match target {
        PlanTarget::Agent { agent_id } => serde_json::json!({"agent_id": agent_id}),
        PlanTarget::Workflow { workflow_id } => serde_json::json!({"workflow_id": workflow_id}),
        PlanTarget::PluginPlan { config, .. } => config.clone(),
    }
}
pub(crate) fn public_payload(message: &Message) -> Result<Value, ProtocolError> {
    let mut value =
        serde_json::to_value(&message.payload).map_err(|_| ProtocolError::PayloadMismatch)?;
    let target = match &message.payload {
        MessagePayload::Request(RequestPayload::SessionCreate(p)) => Some(&p.target),
        MessagePayload::Request(RequestPayload::SessionExecutionStart(p)) => p.target.as_ref(),
        MessagePayload::Request(RequestPayload::SessionConfigUpdate(p)) => p.target.as_ref(),
        _ => return Ok(value),
    };
    let object = value
        .as_object_mut()
        .ok_or(ProtocolError::PayloadMismatch)?;
    object.remove("target");
    if let Some(target) = target {
        if matches!(target, PlanTarget::PluginPlan { .. })
            && matches!(
                message.payload,
                MessagePayload::Request(RequestPayload::SessionExecutionStart(_))
            )
        {
            object.remove("input");
        }
        let fields = target_payload(target);
        let fields = fields.as_object().ok_or(ProtocolError::InvalidField(
            "session Plan config must be an object",
        ))?;
        for (key, value) in fields {
            object.insert(key.clone(), value.clone());
        }
    }
    Ok(value)
}
pub(crate) fn adapt_public_payload(
    event: MessageEvent,
    plan: Option<&PlanRef>,
    mut value: Value,
) -> Result<Value, ProtocolError> {
    let create = event == MessageEvent::Request(RequestEvent::SessionCreate);
    let start = event == MessageEvent::Request(RequestEvent::SessionExecutionStart);
    let update = event == MessageEvent::Request(RequestEvent::SessionConfigUpdate);
    if !create && !start && !update {
        return Ok(value);
    }
    let fields = value
        .as_object_mut()
        .ok_or(ProtocolError::PayloadMismatch)?;
    if fields.contains_key("target") {
        return Err(ProtocolError::InvalidField("payload.target"));
    }
    let Some(plan) = plan else {
        if create {
            return Err(ProtocolError::InvalidField("plan"));
        }
        return Ok(value);
    };
    let target = match plan.plan_id.as_str() {
        "single_agent" => PlanTarget::Agent {
            agent_id: serde_json::from_value(
                fields
                    .remove("agent_id")
                    .ok_or(ProtocolError::InvalidField("agent_id"))?,
            )
            .map_err(|_| ProtocolError::InvalidField("agent_id"))?,
        },
        "workflow" => PlanTarget::Workflow {
            workflow_id: serde_json::from_value(
                fields
                    .remove("workflow_id")
                    .ok_or(ProtocolError::InvalidField("workflow_id"))?,
            )
            .map_err(|_| ProtocolError::InvalidField("workflow_id"))?,
        },
        id => {
            let controls = if create {
                &["name", "workspace", "options", "channels"][..]
            } else if update {
                &["name", "workspace", "options"][..]
            } else {
                &["workspace", "options"][..]
            };
            let keys = fields
                .keys()
                .filter(|key| !controls.contains(&key.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let mut config = serde_json::Map::new();
            for key in keys {
                config.insert(key.clone(), fields.remove(&key).unwrap());
            }
            if start {
                fields.insert("input".into(), serde_json::json!([]));
            }
            PlanTarget::PluginPlan {
                plan_id: id.into(),
                config: Value::Object(config),
            }
        }
    };
    fields.insert(
        "target".into(),
        serde_json::to_value(target).map_err(|_| ProtocolError::PayloadMismatch)?,
    );
    Ok(value)
}
