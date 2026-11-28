//! Shared 0.1.0 envelopes. Response contracts are selected by the original request.

mod codec;
mod content;
mod event;
mod payload;
mod plugin;
mod request;
mod worker;

pub use codec::*;
pub use content::*;
pub use event::*;
pub use payload::*;
pub use plugin::*;
pub use request::*;
pub use worker::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolApiVersion;
pub const API_VERSION: ProtocolApiVersion = ProtocolApiVersion;
impl std::fmt::Display for ProtocolApiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("0.1.0")
    }
}
impl serde::Serialize for ProtocolApiVersion {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("0.1.0")
    }
}
impl<'de> serde::Deserialize<'de> for ProtocolApiVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let version = <String as serde::Deserialize>::deserialize(d)?;
        if version == "0.1.0" {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(
                "only api_version 0.1.0 is supported",
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRef {
    pub session_id: crate::SessionId,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRef {
    pub plan_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCallRef {
    pub runtime_id: String,
}

/// Server transports carry either a correlated response or a sequenced event.
/// Both alternatives use the same validated 0.1.0 codecs as their standalone types.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum ServerFrame {
    Response(ResponseMessage),
    Event(Message),
}

impl<'de> serde::Deserialize<'de> for ServerFrame {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
        if value.get("response_id").is_some() {
            serde_json::from_value(value)
                .map(Self::Response)
                .map_err(serde::de::Error::custom)
        } else {
            let event: Message = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            if !matches!(event.payload, MessagePayload::Event(_)) {
                return Err(serde::de::Error::custom(
                    "server frame must be a response or event",
                ));
            }
            Ok(Self::Event(event))
        }
    }
}

#[derive(Debug, Clone)]
pub struct Message {
    pub api_version: ProtocolApiVersion,
    pub request_id: Option<crate::RequestId>,
    pub session_id: Option<crate::SessionId>,
    pub stream_id: Option<String>,
    pub seq: Option<u64>,
    pub event: MessageEvent,
    pub payload: MessagePayload,
    pub plan: Option<PlanRef>,
    pub call: Option<RuntimeCallRef>,
}

impl Message {
    pub fn request(
        request_id: crate::RequestId,
        session_id: Option<crate::SessionId>,
        payload: RequestPayload,
    ) -> Result<Self, ProtocolError> {
        let plan = match &payload {
            RequestPayload::SessionCreate(p) => Some(target_plan_ref(&p.target)),
            RequestPayload::SessionExecutionStart(p) => p.target.as_ref().map(target_plan_ref),
            RequestPayload::SessionConfigUpdate(p) => p.target.as_ref().map(target_plan_ref),
            _ => None,
        };
        let message = Self {
            api_version: API_VERSION,
            request_id: Some(request_id),
            session_id,
            stream_id: None,
            seq: None,
            event: MessageEvent::Request(payload.event()),
            payload: MessagePayload::Request(payload),
            plan,
            call: None,
        };
        message.validate()?;
        Ok(message)
    }

    pub fn event(
        session_id: crate::SessionId,
        stream_id: String,
        seq: u64,
        request_id: Option<crate::RequestId>,
        payload: EventPayload,
    ) -> Result<Self, ProtocolError> {
        let message = Self {
            api_version: API_VERSION,
            request_id,
            session_id: Some(session_id),
            stream_id: Some(stream_id),
            seq: Some(seq),
            event: MessageEvent::Event(payload.event()),
            payload: MessagePayload::Event(payload),
            plan: None,
            call: None,
        };
        message.validate()?;
        Ok(message)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseMessage {
    pub api_version: ProtocolApiVersion,
    pub response_id: crate::RequestId,
    pub code: ResponseCode,
    pub msg: String,
    pub payload: serde_json::Value,
    pub session: Option<SessionRef>,
    pub plan: Option<PlanRef>,
}

impl<'de> serde::Deserialize<'de> for ResponseMessage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            api_version: ProtocolApiVersion,
            response_id: crate::RequestId,
            code: ResponseCode,
            msg: String,
            payload: serde_json::Value,
            session: Option<SessionRef>,
            plan: Option<PlanRef>,
        }
        let value = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
        for key in [
            "api_version",
            "response_id",
            "code",
            "msg",
            "payload",
            "session",
            "plan",
        ] {
            if value.get(key).is_none() {
                return Err(serde::de::Error::custom(format!("missing {key}")));
            }
        }
        let wire: Wire = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        let response = Self {
            api_version: wire.api_version,
            response_id: wire.response_id,
            code: wire.code,
            msg: wire.msg,
            payload: wire.payload,
            session: wire.session,
            plan: wire.plan,
        };
        response.validate().map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

impl ResponseMessage {
    pub fn success(
        request: &Message,
        session_id: Option<crate::SessionId>,
        msg: impl Into<String>,
        data: Option<ResponsePayload>,
    ) -> Result<Self, ProtocolError> {
        let payload = request.as_request()?;
        if payload.event() != RequestEvent::SessionCreate && session_id != request.session_id {
            return Err(ProtocolError::InvalidField("response session_id"));
        }
        let response = Self {
            api_version: API_VERSION,
            response_id: request
                .request_id
                .clone()
                .ok_or(ProtocolError::InvalidField("request_id"))?,
            session: session_id.map(|session_id| SessionRef { session_id }),
            plan: request.plan.clone(),
            code: ResponseCode::Success,
            msg: msg.into(),
            payload: serde_json::to_value(data).map_err(|_| ProtocolError::PayloadMismatch)?,
        };
        response.validate()?;
        Ok(response)
    }

    pub fn error(
        request: &Message,
        code: ResponseCode,
        msg: impl Into<String>,
    ) -> Result<Self, ProtocolError> {
        let mut response = Self::success(request, request.session_id.clone(), msg, None)?;
        response.code = code;
        response.validate()?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests;
