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
        match self.data {
            SessionEventData::NodeCompleted { .. } => "node_completed",
            SessionEventData::TurnStarted { .. } => "turn_started",
            SessionEventData::HistoryLoaded { .. } => "history_loaded",
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

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEventData {
    NodeCompleted {
        output: Value,
        #[serde(default)]
        finished: bool,
    },
    TurnStarted {
        input: String,
    },
    HistoryLoaded {
        messages: Vec<SessionMessage>,
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
    fn custom_event_preserves_type_and_content() {
        let event = SessionEvent::workflow(
            "workflow",
            "node",
            SessionEventData::Custom {
                event_type: "progress".to_string(),
                content: json!({"percent": 50}),
            },
        );

        assert_eq!(event.kind(), "custom");
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "workflow_id": "workflow",
                "node_id": "node",
                "name": "node",
                "type": "custom",
                "event_type": "progress",
                "content": {"percent": 50}
            })
        );
    }
}
