//! Host-owned definition and channel storage. These are never protocol frames.
use fae_agent::SingleAgentSource;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DefinitionTarget {
    Agent { source: SingleAgentSource },
    Workflow { workflow_id: String, input: Value },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredSessionSpec {
    pub target: DefinitionTarget,
    pub session_id: Option<String>,
    pub workspace: Option<PathBuf>,
    pub resume: Option<ReplayCursor>,
    pub binding: Option<LarkPrincipal>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReplayCursor {
    pub stream_id: String,
    pub after_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LarkPrincipal {
    #[serde(default = "legacy_mapping_version")]
    pub session_mapping_version: u16,
    #[serde(default)]
    pub thread_id: String,
    pub channel: String,
    pub user_id: String,
    pub app_id: String,
    pub tenant_key: String,
    pub chat_type: String,
    pub chat_id: String,
}
pub fn legacy_mapping_version() -> u16 {
    1
}

pub struct SessionFacts {
    pub target_id: String,
    pub name: String,
    pub user_id: String,
    pub model: String,
    pub session_id: String,
    pub workspace: PathBuf,
}

/// Persisted timer and gateway receipt outcome; its existing storage format is retained.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredOutcome {
    Succeeded { result: Value },
    Failed { message: String },
    Cancelled { reason: String },
}
impl StoredOutcome {
    pub fn public(&self) -> fae_agent::ExecutionOutcome {
        use fae_agent::protocol::{ContentBlock, ErrorInfo};
        match self {
            Self::Succeeded { result } => fae_agent::ExecutionOutcome::Succeeded {
                result: vec![ContentBlock::Json {
                    value: result.clone(),
                }],
            },
            Self::Failed { message } => fae_agent::ExecutionOutcome::Failed {
                error: ErrorInfo {
                    code: "execution_failed".into(),
                    message: message.clone(),
                },
            },
            Self::Cancelled { reason } => fae_agent::ExecutionOutcome::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}
