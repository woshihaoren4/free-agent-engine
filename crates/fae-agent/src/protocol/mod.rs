//! The current shared protocol. Only v4 is accepted; transport and authentication belong to hosts.

mod codec;
mod content;
mod path;
mod payload;
mod plugin;

pub use codec::*;
pub use content::*;
pub use path::*;
pub use payload::*;
pub use plugin::*;

pub const VERSION: u16 = 4;

/// Server transports carry either a correlated response or a sequenced event.
/// Both alternatives use the same validated v4 codecs as their standalone types.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum ServerFrame {
    Response(ResponseMessage),
    Event(Message),
}

impl<'de> serde::Deserialize<'de> for ServerFrame {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
        if value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| path.starts_with("/response/"))
        {
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
    pub version: u16,
    pub request_id: Option<crate::RequestId>,
    pub session_id: Option<crate::SessionId>,
    pub stream_id: Option<String>,
    pub seq: Option<u64>,
    pub path: MessagePath,
    pub payload: MessagePayload,
}

impl Message {
    pub fn request(
        request_id: crate::RequestId,
        session_id: Option<crate::SessionId>,
        payload: RequestPayload,
    ) -> Result<Self, ProtocolError> {
        let message = Self {
            version: VERSION,
            request_id: Some(request_id),
            session_id,
            stream_id: None,
            seq: None,
            path: MessagePath::Request(payload.path()),
            payload: MessagePayload::Request(payload),
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
            version: VERSION,
            request_id,
            session_id: Some(session_id),
            stream_id: Some(stream_id),
            seq: Some(seq),
            path: MessagePath::Event(payload.path()),
            payload: MessagePayload::Event(payload),
        };
        message.validate()?;
        Ok(message)
    }
}

#[derive(Debug, Clone)]
pub struct ResponseMessage {
    pub version: u16,
    pub request_id: crate::RequestId,
    pub session_id: Option<crate::SessionId>,
    pub path: ResponsePath,
    pub payload: ResponseBody,
}

#[derive(Debug, Clone)]
pub struct ResponseBody {
    pub code: ResponseCode,
    pub msg: String,
    pub data: Option<ResponsePayload>,
}

impl ResponseMessage {
    pub fn success(
        request: &Message,
        session_id: Option<crate::SessionId>,
        msg: impl Into<String>,
        data: Option<ResponsePayload>,
    ) -> Result<Self, ProtocolError> {
        let payload = request.as_request()?;
        if payload.path() != RequestPath::SessionCreate && session_id != request.session_id {
            return Err(ProtocolError::InvalidField("response session_id"));
        }
        let response = Self {
            version: VERSION,
            request_id: request
                .request_id
                .clone()
                .ok_or(ProtocolError::InvalidField("request_id"))?,
            session_id,
            path: payload.path().response(),
            payload: ResponseBody {
                code: ResponseCode::Success,
                msg: msg.into(),
                data,
            },
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
        response.payload.code = code;
        response.validate()?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests;
