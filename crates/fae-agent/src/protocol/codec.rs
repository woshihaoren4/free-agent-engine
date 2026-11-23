use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{RequestId, SessionId, session::MAX_ID_BYTES};

use super::{
    Message, MessagePath, MessagePayload, RequestPayload, ResponseBody, ResponseMessage,
    ResponsePayload, VERSION,
};

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
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u16(*self as u16)
    }
}

impl<'de> Deserialize<'de> for ResponseCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u16::deserialize(deserializer)? {
            0 => Ok(Self::Success),
            400 => Ok(Self::BadRequest),
            403 => Ok(Self::Forbidden),
            404 => Ok(Self::NotFound),
            409 => Ok(Self::Conflict),
            500 => Ok(Self::InternalError),
            503 => Ok(Self::Unavailable),
            507 => Ok(Self::StorageError),
            code => Err(serde::de::Error::custom(format!(
                "unregistered response code {code}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("unsupported protocol version {0}; expected {VERSION}")]
    UnsupportedVersion(u16),
    #[error("path and payload do not match")]
    PayloadMismatch,
    #[error("invalid envelope field: {0}")]
    InvalidField(&'static str),
    #[error("responses with an error code must have data=null")]
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

fn version(value: &Value) -> Result<(), String> {
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "version must be an unsigned integer".to_string())?;
    if version != u64::from(VERSION) {
        return Err(format!(
            "unsupported protocol version {version}; expected {VERSION}"
        ));
    }
    Ok(())
}

impl Message {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != VERSION {
            return Err(ProtocolError::UnsupportedVersion(self.version));
        }
        if self.path != self.payload.path() {
            return Err(ProtocolError::PayloadMismatch);
        }
        match &self.payload {
            MessagePayload::Request(request) => {
                if self.request_id.is_none() {
                    return Err(ProtocolError::InvalidField("request_id"));
                }
                let path = request.path();
                request.validate_plugin(self.session_id.as_ref())?;
                if !path.is_plugin() && path.requires_session() != self.session_id.is_some() {
                    return Err(ProtocolError::InvalidField("session_id"));
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
                if let RequestPayload::EngineHello(hello) = request {
                    if !hello.versions.contains(&VERSION) {
                        return Err(ProtocolError::UnsupportedVersion(
                            hello.versions.first().copied().unwrap_or(0),
                        ));
                    }
                }
            }
            MessagePayload::Event(_) => {
                if self.session_id.is_none() {
                    return Err(ProtocolError::InvalidField("session_id"));
                }
                identifier(
                    self.stream_id
                        .as_deref()
                        .ok_or(ProtocolError::InvalidField("stream_id"))?,
                    "stream_id",
                )?;
                if self.seq.is_none_or(|seq| seq == 0) {
                    return Err(ProtocolError::InvalidField("seq"));
                }
            }
        }
        Ok(())
    }

    /// Called by a host request dispatcher after decoding; event emission requires a trusted source.
    pub fn as_request(&self) -> Result<&RequestPayload, ProtocolError> {
        self.validate()?;
        match &self.payload {
            MessagePayload::Request(request) => Ok(request),
            MessagePayload::Event(_) => Err(ProtocolError::ExternalEvent),
        }
    }
}

impl ResponseMessage {
    /// Method-discriminated plugin values must be checked against the original request.
    pub fn validate_for(&self, message: &Message) -> Result<(), ProtocolError> {
        self.validate()?;
        let request = message.as_request()?;
        if Some(&self.request_id) != message.request_id.as_ref()
            || self.path != request.path().response()
            || (self.session_id != message.session_id
                && !(request.path() == super::RequestPath::SessionCreate
                    && self.payload.code == ResponseCode::Success))
        {
            return Err(ProtocolError::PayloadMismatch);
        }
        if self.payload.code == ResponseCode::Success {
            let hook = match request {
                RequestPayload::PluginInvoke(p) => Some((&p.point, &p.method)),
                RequestPayload::PluginNext(p) => Some((&p.point, &p.method)),
                _ => None,
            };
            if let Some((point, method)) = hook {
                let value = serde_json::to_value(&self.payload.data)
                    .map_err(|_| ProtocolError::PayloadMismatch)?;
                super::validate_hook_value(point, method, &value, true)?;
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != VERSION {
            return Err(ProtocolError::UnsupportedVersion(self.version));
        }
        let request = self.path.request();
        if request.requires_session() && self.session_id.is_none() {
            return Err(ProtocolError::InvalidField("session_id"));
        }
        if !request.is_session() && !request.is_plugin() && self.session_id.is_some() {
            return Err(ProtocolError::InvalidField("session_id"));
        }
        if self.payload.code != ResponseCode::Success && self.payload.data.is_some() {
            return Err(ProtocolError::ErrorData);
        }
        if let Some(payload) = &self.payload.data {
            if self.path != payload.path() {
                return Err(ProtocolError::PayloadMismatch);
            }
            if let ResponsePayload::SessionCreate(summary) = payload {
                if self.session_id.as_ref() != Some(&summary.session_config.session_id) {
                    return Err(ProtocolError::InvalidField("created session_id"));
                }
            }
            if let ResponsePayload::EngineHello(hello) = payload {
                if hello.version != VERSION {
                    return Err(ProtocolError::UnsupportedVersion(hello.version));
                }
            }
            if let ResponsePayload::SessionExecutionSteer(accepted) = payload {
                if accepted.request_id != self.request_id {
                    return Err(ProtocolError::InvalidField("input request_id"));
                }
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireMessage {
    version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_id: Option<RequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stream_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    path: MessagePath,
    payload: Value,
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(serde::ser::Error::custom)?;
        WireMessage {
            version: self.version,
            request_id: self.request_id.clone(),
            session_id: self.session_id.clone(),
            stream_id: self.stream_id.clone(),
            seq: self.seq,
            path: self.path,
            payload: serde_json::to_value(&self.payload).map_err(serde::ser::Error::custom)?,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        version(&value).map_err(serde::de::Error::custom)?;
        let wire: WireMessage =
            serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?;
        if matches!(wire.path, MessagePath::Request(_))
            && (value.get("stream_id").is_some() || value.get("seq").is_some())
        {
            return Err(serde::de::Error::custom(
                "request must omit stream_id and seq",
            ));
        }
        let payload = match wire.path {
            MessagePath::Request(path) => MessagePayload::Request(
                RequestPayload::decode(path, wire.payload).map_err(serde::de::Error::custom)?,
            ),
            MessagePath::Event(path) => MessagePayload::Event(
                super::EventPayload::decode(path, wire.payload)
                    .map_err(serde::de::Error::custom)?,
            ),
        };
        let message = Self {
            version: wire.version,
            request_id: wire.request_id,
            session_id: wire.session_id,
            stream_id: wire.stream_id,
            seq: wire.seq,
            path: wire.path,
            payload,
        };
        message.validate().map_err(serde::de::Error::custom)?;
        Ok(message)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResponse {
    version: u16,
    request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<SessionId>,
    path: super::ResponsePath,
    payload: WireBody,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireBody {
    code: ResponseCode,
    msg: String,
    // Value deliberately requires the key to exist, even when its value is null.
    data: Value,
}

impl Serialize for ResponseMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(serde::ser::Error::custom)?;
        WireResponse {
            version: self.version,
            request_id: self.request_id.clone(),
            session_id: self.session_id.clone(),
            path: self.path,
            payload: WireBody {
                code: self.payload.code,
                msg: self.payload.msg.clone(),
                data: serde_json::to_value(&self.payload.data)
                    .map_err(serde::ser::Error::custom)?,
            },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResponseMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        version(&value).map_err(serde::de::Error::custom)?;
        let wire: WireResponse = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        let data = if wire.payload.data.is_null() {
            None
        } else {
            Some(
                ResponsePayload::decode(wire.path, wire.payload.data)
                    .map_err(serde::de::Error::custom)?,
            )
        };
        let response = Self {
            version: wire.version,
            request_id: wire.request_id,
            session_id: wire.session_id,
            path: wire.path,
            payload: ResponseBody {
                code: wire.payload.code,
                msg: wire.payload.msg,
                data,
            },
        };
        response.validate().map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}
