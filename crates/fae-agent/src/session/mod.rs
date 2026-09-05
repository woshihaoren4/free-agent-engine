use std::{fmt::Debug, path::PathBuf, sync::Arc};

use serde_json::Value;
use tokio::sync::{
    Mutex,
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

#[async_trait::async_trait]
pub trait Session<In, Out>: Debug + Send + Sync + 'static {
    async fn call(&self, input: In) -> anyhow::Result<()>;
    async fn answer(&self) -> anyhow::Result<Option<Out>>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionMessage {
    pub role: SessionMessageRole,
    pub content: String,
}

impl SessionMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: SessionMessageRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: SessionMessageRole::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<u64>,
    #[serde(rename = "name")]
    pub source: String,
    #[serde(flatten)]
    pub data: SessionEventData,
}

impl SessionEvent {
    pub fn single_agent(turn_id: u64, source: impl Into<String>, data: SessionEventData) -> Self {
        Self {
            workflow_id: None,
            node_id: None,
            turn_id: Some(turn_id),
            source: source.into(),
            data,
        }
    }

    pub fn workflow(
        workflow_id: impl Into<String>,
        node_id: impl Into<String>,
        data: SessionEventData,
    ) -> Self {
        let node_id = node_id.into();
        Self {
            workflow_id: Some(workflow_id.into()),
            source: node_id.clone(),
            node_id: Some(node_id),
            turn_id: None,
            data,
        }
    }

    pub fn in_workflow(
        workflow_id: impl Into<String>,
        node_id: impl Into<String>,
        turn_id: u64,
        source: impl Into<String>,
        data: SessionEventData,
    ) -> Self {
        Self {
            workflow_id: Some(workflow_id.into()),
            node_id: Some(node_id.into()),
            turn_id: Some(turn_id),
            source: source.into(),
            data,
        }
    }

    pub fn kind(&self) -> &'static str {
        self.data.kind()
    }

    pub fn event_type(&self) -> &str {
        self.data.event_type()
    }

    pub fn name(&self) -> &str {
        &self.source
    }

    pub fn turn_id(&self) -> Option<u64> {
        self.turn_id
    }

    pub fn is_terminal(&self) -> bool {
        if self.workflow_id.is_some() {
            matches!(
                self.data,
                SessionEventData::NodeCompleted { finished: true, .. }
                    | SessionEventData::Failed { .. } if self.turn_id.is_none()
            )
        } else {
            matches!(
                self.data,
                SessionEventData::Completed { .. } | SessionEventData::Failed { .. }
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEventData {
    NodeCompleted {
        output: Value,
        finished: bool,
    },
    TurnStarted {
        input: String,
    },
    UserInput {
        content: String,
    },
    ModelOutput {
        content: String,
    },
    ModelReasoning {
        content: String,
    },
    ToolCall {
        call_id: String,
        arguments: String,
    },
    ToolOutput {
        call_id: String,
        output: String,
        completed: bool,
    },
    Completed {
        content: String,
    },
    Failed {
        error: String,
    },
    Custom {
        event_type: String,
        content: Value,
    },
}

impl SessionEventData {
    pub fn kind(&self) -> &'static str {
        match self {
            SessionEventData::NodeCompleted { .. } => "node_completed",
            SessionEventData::TurnStarted { .. } => "turn_started",
            SessionEventData::UserInput { .. } => "user_input",
            SessionEventData::ModelOutput { .. } => "model_output",
            SessionEventData::ModelReasoning { .. } => "model_reasoning",
            SessionEventData::ToolCall { .. } => "tool_call",
            SessionEventData::ToolOutput { .. } => "tool_output",
            SessionEventData::Completed { .. } => "completed",
            SessionEventData::Failed { .. } => "failed",
            SessionEventData::Custom { .. } => "custom",
        }
    }

    pub fn event_type(&self) -> &str {
        match self {
            Self::Custom { event_type, .. } => event_type,
            _ => self.kind(),
        }
    }

    fn content(&self) -> Value {
        match self {
            Self::NodeCompleted { output, finished } => {
                serde_json::json!({ "output": output, "finished": finished })
            }
            Self::TurnStarted { input } => serde_json::json!({ "input": input }),
            Self::UserInput { content }
            | Self::ModelOutput { content }
            | Self::ModelReasoning { content }
            | Self::Completed { content } => serde_json::json!({ "content": content }),
            Self::ToolCall { call_id, arguments } => {
                serde_json::json!({ "call_id": call_id, "arguments": arguments })
            }
            Self::ToolOutput {
                call_id,
                output,
                completed,
            } => serde_json::json!({
                "call_id": call_id,
                "output": output,
                "completed": completed,
            }),
            Self::Failed { error } => serde_json::json!({ "error": error }),
            Self::Custom { content, .. } => content.clone(),
        }
    }
}

impl serde::Serialize for SessionEventData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut state = serializer.serialize_struct("SessionEventData", 2)?;
        state.serialize_field("type", self.event_type())?;
        state.serialize_field("data", &self.content())?;
        state.end()
    }
}

impl<'de> serde::Deserialize<'de> for SessionEventData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <Value as serde::Deserialize>::deserialize(deserializer)?;
        deserialize_event_data(value).map_err(serde::de::Error::custom)
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KnownSessionEventData {
    NodeCompleted {
        output: Value,
        #[serde(default)]
        finished: bool,
    },
    TurnStarted {
        input: String,
    },
    UserInput {
        content: String,
    },
    ModelOutput {
        content: String,
    },
    ModelReasoning {
        content: String,
    },
    ToolCall {
        call_id: String,
        arguments: String,
    },
    ToolOutput {
        call_id: String,
        output: String,
        completed: bool,
    },
    Completed {
        content: String,
    },
    Failed {
        error: String,
    },
}

impl From<KnownSessionEventData> for SessionEventData {
    fn from(data: KnownSessionEventData) -> Self {
        match data {
            KnownSessionEventData::NodeCompleted { output, finished } => {
                Self::NodeCompleted { output, finished }
            }
            KnownSessionEventData::TurnStarted { input } => Self::TurnStarted { input },
            KnownSessionEventData::UserInput { content } => Self::UserInput { content },
            KnownSessionEventData::ModelOutput { content } => Self::ModelOutput { content },
            KnownSessionEventData::ModelReasoning { content } => Self::ModelReasoning { content },
            KnownSessionEventData::ToolCall { call_id, arguments } => {
                Self::ToolCall { call_id, arguments }
            }
            KnownSessionEventData::ToolOutput {
                call_id,
                output,
                completed,
            } => Self::ToolOutput {
                call_id,
                output,
                completed,
            },
            KnownSessionEventData::Completed { content } => Self::Completed { content },
            KnownSessionEventData::Failed { error } => Self::Failed { error },
        }
    }
}

fn deserialize_event_data(value: Value) -> serde_json::Result<SessionEventData> {
    let Value::Object(mut event) = value else {
        return Err(serde::de::Error::custom(
            "session event data must be an object",
        ));
    };
    let event_type = event
        .remove("type")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| serde::de::Error::missing_field("type"))?;

    let is_known = matches!(
        event_type.as_str(),
        "node_completed"
            | "turn_started"
            | "user_input"
            | "model_output"
            | "model_reasoning"
            | "tool_call"
            | "tool_output"
            | "completed"
            | "failed"
    );

    if let Some(content) = event.remove("data") {
        if !is_known {
            return Ok(SessionEventData::Custom {
                event_type,
                content,
            });
        }
        let Value::Object(mut content) = content else {
            return Err(serde::de::Error::custom(format!(
                "data for built-in event type `{event_type}` must be an object"
            )));
        };
        content.insert("type".to_string(), Value::String(event_type));
        return serde_json::from_value::<KnownSessionEventData>(Value::Object(content))
            .map(Into::into);
    }

    // Accept the original flat representation when reading persisted or in-flight events.
    if event_type == "custom" {
        let event_type = event
            .remove("event_type")
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| serde::de::Error::missing_field("event_type"))?;
        let content = event
            .remove("content")
            .ok_or_else(|| serde::de::Error::missing_field("content"))?;
        return Ok(SessionEventData::Custom {
            event_type,
            content,
        });
    }
    if !is_known {
        return Ok(SessionEventData::Custom {
            event_type,
            content: Value::Object(event),
        });
    }

    event.insert("type".to_string(), Value::String(event_type));
    serde_json::from_value::<KnownSessionEventData>(Value::Object(event)).map(Into::into)
}

#[derive(Debug, Clone)]
pub(crate) struct SessionEventChannel {
    inner: Arc<SessionEventChannelInner>,
}

#[derive(Debug)]
struct SessionEventChannelInner {
    sender: UnboundedSender<SessionEvent>,
    receiver: Mutex<UnboundedReceiver<SessionEvent>>,
}

impl SessionEventChannel {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = unbounded_channel();
        Self {
            inner: Arc::new(SessionEventChannelInner {
                sender,
                receiver: Mutex::new(receiver),
            }),
        }
    }

    pub(crate) fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        self.inner
            .sender
            .send(event)
            .map_err(|error| anyhow::anyhow!("send session event failed: {error}"))
    }

    pub(crate) async fn answer(&self) -> Option<SessionEvent> {
        self.inner.receiver.lock().await.recv().await
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionQuery {
    pub user: String,
    pub session_id: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

impl SessionQuery {
    pub fn new(user: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            session_id: session_id.into(),
            limit: None,
            offset: None,
        }
    }

    pub fn with_page(
        user: impl Into<String>,
        session_id: impl Into<String>,
        limit: impl Into<Option<usize>>,
        offset: impl Into<Option<usize>>,
    ) -> Self {
        Self {
            user: user.into(),
            session_id: session_id.into(),
            limit: limit.into(),
            offset: offset.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionRequest {
    Add {
        user: String,
        session_id: String,
        messages: Vec<SessionMessage>,
    },
    Delete {
        user: String,
        session_id: String,
    },
    Query {
        user: String,
        session_id: String,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionResponse {
    Added {
        path: PathBuf,
        added: usize,
    },
    Deleted {
        path: PathBuf,
        existed: bool,
    },
    History {
        path: PathBuf,
        messages: Vec<SessionMessage>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn custom_event_uses_its_type_and_supports_arbitrary_content() {
        let event = SessionEvent::workflow(
            "workflow",
            "node",
            SessionEventData::Custom {
                event_type: "progress".to_string(),
                content: json!({"percent": 50}),
            },
        );

        assert_eq!(event.kind(), "custom");
        assert_eq!(event.event_type(), "progress");
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "workflow_id": "workflow",
                "node_id": "node",
                "name": "node",
                "type": "progress",
                "data": {"percent": 50}
            })
        );
        assert_eq!(
            serde_json::from_value::<SessionEvent>(serde_json::to_value(&event).unwrap()).unwrap(),
            event
        );
    }

    #[test]
    fn event_deserializes_from_legacy_flat_format() {
        let event = serde_json::from_value::<SessionEvent>(json!({
            "turn_id": 7,
            "name": "read_file",
            "type": "tool_call",
            "call_id": "call-1",
            "arguments": "{}"
        }))
        .unwrap();

        assert_eq!(
            event.data,
            SessionEventData::ToolCall {
                call_id: "call-1".to_string(),
                arguments: "{}".to_string(),
            }
        );
    }

    #[test]
    fn unknown_event_type_deserializes_as_custom() {
        for content in [
            Value::Null,
            json!(true),
            json!(42),
            json!("ready"),
            json!([1, 2, 3]),
            json!({"percent": 50}),
        ] {
            let event = serde_json::from_value::<SessionEvent>(json!({
                "name": "worker",
                "type": "metric",
                "data": content
            }))
            .unwrap();

            assert_eq!(event.kind(), "custom");
            assert_eq!(event.event_type(), "metric");
            assert_eq!(
                event.data,
                SessionEventData::Custom {
                    event_type: "metric".to_string(),
                    content,
                }
            );
        }
    }

    #[test]
    fn custom_event_deserializes_from_legacy_flat_format() {
        let event = serde_json::from_value::<SessionEvent>(json!({
            "name": "worker",
            "type": "custom",
            "event_type": "progress",
            "content": {"percent": 50}
        }))
        .unwrap();

        assert_eq!(event.event_type(), "progress");
        assert_eq!(
            event.data,
            SessionEventData::Custom {
                event_type: "progress".to_string(),
                content: json!({"percent": 50}),
            }
        );
    }
}
