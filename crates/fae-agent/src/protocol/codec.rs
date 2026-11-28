use super::*;
use crate::{RequestId, SessionId, session::MAX_ID_BYTES};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ResponseCode {
    Success = 0,
    BadRequest = 400,
    Forbidden = 403,
    NotFound = 404,
    Conflict = 409,
    InternalError = 500,
    Unavailable = 503,
    StorageError = 507,
}
impl ResponseCode {
    pub const ALL: &'static [Self] = &[
        Self::Success,
        Self::BadRequest,
        Self::Forbidden,
        Self::NotFound,
        Self::Conflict,
        Self::InternalError,
        Self::Unavailable,
        Self::StorageError,
    ];
}
impl Serialize for ResponseCode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u16(*self as u16)
    }
}
impl<'de> Deserialize<'de> for ResponseCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let code = u16::deserialize(d)?;
        Self::ALL
            .iter()
            .copied()
            .find(|v| *v as u16 == code)
            .ok_or_else(|| serde::de::Error::custom(format!("unregistered response code {code}")))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("event and payload do not match")]
    PayloadMismatch,
    #[error("invalid envelope field: {0}")]
    InvalidField(&'static str),
    #[error("responses with an error code must have payload=null")]
    ErrorData,
    #[error("external events cannot be executed as requests")]
    ExternalEvent,
}
impl ProtocolError {
    pub fn code(&self) -> ResponseCode {
        ResponseCode::BadRequest
    }
}
fn identifier(value: &str, field: &'static str) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_ID_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProtocolError::InvalidField(field));
    }
    Ok(())
}
impl Message {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.event != self.payload.event() {
            return Err(ProtocolError::PayloadMismatch);
        }
        if let Some(plan) = &self.plan {
            crate::plugin::validate_definition_id(&plan.plan_id)
                .map_err(|_| ProtocolError::InvalidField("plan"))?;
        }
        if let Some(call) = &self.call {
            crate::plugin::validate_definition_id(&call.runtime_id)
                .map_err(|_| ProtocolError::InvalidField("call"))?;
        }
        match &self.payload {
            MessagePayload::Request(request) => {
                if self.request_id.is_none() {
                    return Err(ProtocolError::InvalidField("request_id"));
                }
                let event = request.event();
                match event {
                    RequestEvent::PlanCall
                        if self.plan.is_none()
                            || self.call.is_some()
                            || self.session_id.is_some() =>
                    {
                        return Err(ProtocolError::InvalidField("plan/call route"));
                    }
                    RequestEvent::RuntimeExec | RequestEvent::RuntimeSelect
                        if self.call.is_none()
                            || self.plan.is_some()
                            || self.session_id.is_some() =>
                    {
                        return Err(ProtocolError::InvalidField("runtime route"));
                    }
                    _ => {}
                }
                request.validate_plugin(self.session_id.as_ref())?;
                if !event.is_plugin() && event.requires_session() != self.session_id.is_some() {
                    return Err(ProtocolError::InvalidField("session"));
                }
                if self.stream_id.is_some() || self.seq.is_some() {
                    return Err(ProtocolError::InvalidField("request stream_id/seq"));
                }
                if let RequestPayload::SessionSubscribe(cursor) = request {
                    if cursor.stream_id.is_some() != cursor.after_seq.is_some() {
                        return Err(ProtocolError::InvalidField("subscribe cursor"));
                    }
                    if let Some(stream) = &cursor.stream_id {
                        identifier(stream, "subscribe stream_id")?;
                    }
                }
                if let RequestPayload::SessionUnsubscribe(request) = request {
                    identifier(&request.subscription_id, "subscription_id")?;
                }
                if let RequestPayload::StreamNext(request) | RequestPayload::StreamClose(request) =
                    request
                {
                    identifier(&request.stream_id, "stream_id")?;
                }
            }
            MessagePayload::Event(_) => {
                if self.session_id.is_none() {
                    return Err(ProtocolError::InvalidField("session"));
                }
                identifier(
                    self.stream_id
                        .as_deref()
                        .ok_or(ProtocolError::InvalidField("stream_id"))?,
                    "stream_id",
                )?;
                if self.seq.is_none_or(|v| v == 0) {
                    return Err(ProtocolError::InvalidField("seq"));
                }
            }
        }
        Ok(())
    }
    pub fn as_request(&self) -> Result<&RequestPayload, ProtocolError> {
        self.validate()?;
        match &self.payload {
            MessagePayload::Request(request) => Ok(request),
            MessagePayload::Event(_) => Err(ProtocolError::ExternalEvent),
        }
    }
}
impl ResponseMessage {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.code != ResponseCode::Success && !self.payload.is_null() {
            return Err(ProtocolError::ErrorData);
        }
        if let Some(plan) = &self.plan {
            identifier(&plan.plan_id, "plan")?;
        }
        Ok(())
    }
    pub fn validate_for(&self, message: &Message) -> Result<(), ProtocolError> {
        self.validate()?;
        let request = message.as_request()?;
        let session = self.session.as_ref().map(|s| &s.session_id);
        if Some(&self.response_id) != message.request_id.as_ref()
            || self.plan != message.plan
            || (session != message.session_id.as_ref()
                && !(request.event() == RequestEvent::SessionCreate
                    && self.code == ResponseCode::Success))
        {
            return Err(ProtocolError::PayloadMismatch);
        }
        if self.code == ResponseCode::Success {
            let decoded = self.decode_for(message)?;
            if let Some(ResponsePayload::SessionCreate(summary)) = decoded {
                if session != Some(&summary.session_config.session_id) {
                    return Err(ProtocolError::InvalidField("created session_id"));
                }
            }
            if let Some(ResponsePayload::SessionExecutionSteer(accepted)) =
                self.decode_for(message)?
            {
                if accepted.request_id != self.response_id {
                    return Err(ProtocolError::InvalidField("input request_id"));
                }
            }
            let hook = match request {
                RequestPayload::PluginInvoke(p) => Some((&p.point, &p.method)),
                RequestPayload::PluginNext(p) => Some((&p.point, &p.method)),
                _ => None,
            };
            if let Some((point, method)) = hook {
                validate_hook_value(point, method, &self.payload, true)?;
            }
        }
        Ok(())
    }
    pub fn decode_for(&self, message: &Message) -> Result<Option<ResponsePayload>, ProtocolError> {
        if self.code != ResponseCode::Success {
            return Ok(None);
        }
        if self.payload.is_null()
            && !matches!(
                message.as_request()?.event(),
                RequestEvent::PlanCall
                    | RequestEvent::RuntimeExec
                    | RequestEvent::RuntimeSelect
                    | RequestEvent::StreamNext
            )
        {
            return Ok(None);
        }
        ResponsePayload::decode(message.as_request()?.event(), self.payload.clone())
            .map(Some)
            .map_err(|_| ProtocolError::PayloadMismatch)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NotificationSessionRef {
    session_id: SessionId,
    stream_id: String,
    seq: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireMessage {
    api_version: ProtocolApiVersion,
    request_id: Option<RequestId>,
    event: MessageEvent,
    payload: Value,
    session: Value,
    plan: Option<PlanRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call: Option<RuntimeCallRef>,
}
impl Serialize for Message {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(serde::ser::Error::custom)?;
        let session = match (&self.session_id, &self.stream_id, self.seq) {
            (Some(id), Some(stream), Some(seq)) => serde_json::to_value(NotificationSessionRef {
                session_id: id.clone(),
                stream_id: stream.clone(),
                seq,
            }),
            (Some(id), _, _) => serde_json::to_value(SessionRef {
                session_id: id.clone(),
            }),
            _ => Ok(Value::Null),
        }
        .map_err(serde::ser::Error::custom)?;
        WireMessage {
            api_version: self.api_version,
            request_id: self.request_id.clone(),
            event: self.event,
            payload: public_payload(self).map_err(serde::ser::Error::custom)?,
            session,
            plan: self.plan.clone(),
            call: self.call.clone(),
        }
        .serialize(s)
    }
}
impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
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
        let wire: WireMessage = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        let (session_id, stream_id, seq) = if wire.session.is_null() {
            (None, None, None)
        } else if matches!(wire.event, MessageEvent::Event(_)) {
            let reference: NotificationSessionRef =
                serde_json::from_value(wire.session).map_err(serde::de::Error::custom)?;
            (
                Some(reference.session_id),
                Some(reference.stream_id),
                Some(reference.seq),
            )
        } else {
            let reference: SessionRef =
                serde_json::from_value(wire.session).map_err(serde::de::Error::custom)?;
            (Some(reference.session_id), None, None)
        };
        let adapted = adapt_public_payload(wire.event, wire.plan.as_ref(), wire.payload)
            .map_err(serde::de::Error::custom)?;
        let payload = match wire.event {
            MessageEvent::Request(event) => MessagePayload::Request(
                RequestPayload::decode(event, adapted).map_err(serde::de::Error::custom)?,
            ),
            MessageEvent::Event(event) => MessagePayload::Event(
                EventPayload::decode(event, adapted).map_err(serde::de::Error::custom)?,
            ),
        };
        let message = Self {
            api_version: wire.api_version,
            request_id: wire.request_id,
            session_id,
            stream_id,
            seq,
            event: wire.event,
            payload,
            plan: wire.plan,
            call: wire.call,
        };
        message.validate().map_err(serde::de::Error::custom)?;
        Ok(message)
    }
}
