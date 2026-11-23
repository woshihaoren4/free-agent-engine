//! Serializable Session facts. Runtime handles and mutable execution control live elsewhere.

use serde::{Deserialize, Serialize};

use crate::{ExecutionView, SessionConfig, SessionMessage, UserId, UserMemory};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextData {
    pub user_info: UserInfo,
    pub session_config: SessionConfig,
    pub memory_view: MemoryView,
    pub latest_execution_view: Option<ExecutionView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserInfo {
    pub user_id: UserId,
    pub name: Option<String>,
}

/// A view of existing storage, never a new history or memory storage schema.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryView {
    pub history: Vec<SessionMessage>,
    /// Offset of the loaded window in the default target's existing history partition.
    pub history_offset: usize,
    pub summary: Option<String>,
    pub user_memories: Vec<UserMemory>,
}
