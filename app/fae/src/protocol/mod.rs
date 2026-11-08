mod control;
mod error;
mod session;
pub use control::*;
pub use error::*;
pub use session::*;

pub const VERSION: u16 = 2;
pub const DEFAULT_PORT: u16 = 6666;
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClientEnvelope {
    pub version: u16,
    pub request_id: String,
    #[serde(flatten)]
    pub body: ClientMessage,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerEnvelope {
    pub version: u16,
    pub instance_id: String,
    pub stream_id: Option<String>,
    pub request_id: Option<String>,
    #[serde(flatten)]
    pub body: ServerMessage,
}

impl ServerEnvelope {
    pub fn new(
        instance: &str,
        stream: Option<&str>,
        request: Option<&str>,
        body: ServerMessage,
    ) -> Self {
        Self {
            version: VERSION,
            instance_id: instance.into(),
            stream_id: stream.map(Into::into),
            request_id: request.map(Into::into),
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::{SessionInput, SessionInputData, SessionOutput};
    use serde_json::json;

    #[test]
    fn fixes_input_json_and_preserves_legacy_output_fields() {
        let request = ClientEnvelope {
            version: VERSION,
            request_id: "req-2".into(),
            body: ClientMessage::Input {
                input: SessionInput::NewChat(SessionInputData::text("implement login")),
                mode: Some("todo".into()),
            },
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["type"], "input");
        assert_eq!(
            value["data"]["input"]["new_chat"]["text"],
            "implement login"
        );
        assert_eq!(value["data"]["mode"], "todo");
        let roundtrip: ClientEnvelope = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), value);
        let event = ServerEnvelope::new(
            "instance",
            Some("stream"),
            None,
            ServerMessage::Event {
                seq: 1,
                round_id: "round".into(),
                output: SessionOutput {
                    parament_plan_id: Some("parent".into()),
                    plan_id: Some("plan".into()),
                    node_id: None,
                    agent_name: Some("worker".into()),
                    runtime_id: None,
                    input_type: "custom_progress".into(),
                    output: json!([1, 2, 3]),
                },
            },
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["data"]["output"]["parament_plan_id"], "parent");
        let roundtrip: ServerEnvelope = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), value);
    }

    #[test]
    fn endpoints_use_actual_port_and_map_wildcards() {
        let mut config = ServerConfig {
            instance_id: "test".into(),
            pid: 1,
            address: "0.0.0.0".parse().unwrap(),
            port: 1666,
            protocol_version: VERSION,
            app_version: "test".into(),
            started_at: "".into(),
            executable: "fae".into(),
        };
        assert_eq!(
            config.endpoint("/v1/control"),
            "ws://127.0.0.1:1666/v1/control"
        );
        config.address = "::".parse().unwrap();
        assert_eq!(config.endpoint("/v1/session"), "ws://[::1]:1666/v1/session");
        config.address = "192.168.1.7".parse().unwrap();
        assert_eq!(
            config.endpoint("/v1/session"),
            "ws://192.168.1.7:1666/v1/session"
        );
    }
}
