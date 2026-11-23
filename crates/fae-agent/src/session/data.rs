use serde::{Deserialize, Serialize};

use super::{SessionId, Timestamp};
use crate::context_data::ContextData;

/// Long-lived business data only. Deserialization does not recreate execution resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub context: ContextData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    pub session_id: SessionId,
    pub name: String,
    pub lifecycle: SessionLifecycle,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub target: PlanTarget,
    pub workspace: WorkspaceBinding,
    pub options: ExecutionOptions,
    pub channels: Vec<ChannelBinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    Open,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanTarget {
    /// A resolved definition ID; file sources belong to the definition loader.
    Agent {
        agent_id: String,
    },
    Workflow {
        workflow_id: String,
    },
}

/// The existing allowlisted Agent execution option. None selects normal execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    Todo,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<AgentMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    /// Absolute root resolved by the host. Importers must revalidate this location.
    pub root: String,
    pub project_files: Vec<WorkspaceFileView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFileView {
    pub path: String,
    /// None indicates the file did not exist when read.
    pub content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChannelBinding {
    Tui {
        client_id: String,
    },
    Lark {
        app_id: String,
        chat_id: String,
        thread_id: Option<String>,
    },
    Sdk {
        client_id: String,
    },
}
