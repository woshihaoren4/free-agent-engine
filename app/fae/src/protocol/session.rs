use super::{HelloInfo, ProtocolError};
use fae_agent::{SessionInput, SessionMessage, SessionOutput, SingleAgentSource};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionTarget {
    Agent { source: SingleAgentSource },
    Workflow { workflow_id: String, input: Value },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Resume {
    pub stream_id: String,
    pub after_seq: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OpenSession {
    pub target: SessionTarget,
    pub session_id: Option<String>,
    pub workspace: PathBuf,
    pub resume: Option<Resume>,
    #[serde(default)]
    pub binding: Option<SessionBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionBinding {
    pub channel: String,
    pub user_id: String,
    pub app_id: String,
    pub tenant_key: String,
    pub chat_type: String,
    pub chat_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub target_id: String,
    pub name: String,
    pub user_id: String,
    pub model: String,
    pub session_id: String,
    pub workspace: PathBuf,
    pub status: SessionStatus,
    pub resumed: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionStatus {
    pub state: String,
    pub round_id: Option<String>,
    pub latest_seq: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ClientMessage {
    Open(OpenSession),
    Input {
        input: SessionInput,
        mode: Option<String>,
    },
    ExternalInput {
        external_request_id: String,
        input: SessionInput,
    },
    Acceptance {
        external_request_id: String,
    },
    Cancel {
        round_id: String,
    },
    History {
        limit: usize,
        offset: usize,
    },
    Clean,
    Status,
    Close {
        cancel_active: bool,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello(HelloInfo),
    Initialized {
        config_path: PathBuf,
        skill_count: usize,
    },
    ServiceStatus {
        sessions: usize,
        active_rounds: usize,
    },
    Shutdown,
    Opened(SessionInfo),
    Ack {
        round_id: Option<String>,
    },
    Acceptance {
        round_id: Option<String>,
        uncertain: bool,
        #[serde(default)]
        outcome: Option<RoundOutcome>,
    },
    Event {
        seq: u64,
        round_id: String,
        output: SessionOutput,
    },
    Finished {
        seq: u64,
        round_id: String,
        outcome: RoundOutcome,
    },
    History {
        messages: Vec<SessionMessage>,
    },
    Status(SessionStatus),
    Cleaned,
    Closed,
    Error(ProtocolError),
}

impl ServerMessage {
    pub fn seq(&self) -> Option<u64> {
        match self {
            Self::Event { seq, .. } | Self::Finished { seq, .. } => Some(*seq),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoundOutcome {
    Succeeded { result: Value },
    Failed { message: String },
    Cancelled { reason: String },
}

impl RoundOutcome {
    pub fn result(self) -> anyhow::Result<Value> {
        match self {
            Self::Succeeded { result } => Ok(result),
            Self::Failed { message } => anyhow::bail!("{message}"),
            Self::Cancelled { reason } => anyhow::bail!("Cancelled: {reason}"),
        }
    }
}
