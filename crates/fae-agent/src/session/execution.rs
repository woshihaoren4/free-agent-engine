use serde::{Deserialize, Serialize};

use super::{
    ExecutionId, ExecutionOptions, InvocationId, PlanInstanceId, PlanTarget, RequestId, TaskId,
    Timestamp,
};
use crate::protocol::{Content, ErrorInfo};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionView {
    pub execution_id: ExecutionId,
    pub target: PlanTarget,
    pub effective: ExecutionSettings,
    pub status: ExecutionStatus,
    pub invocations: Vec<InvocationView>,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub outcome: Option<ExecutionOutcome>,
    /// Ordered, repeatable input-processing facts; reading does not consume the mailbox.
    pub inputs: Vec<InputRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSettings {
    pub target: PlanTarget,
    /// The fixed absolute working directory for this Execution.
    pub workspace: String,
    pub options: ExecutionOptions,
    /// References and content fingerprints fixed during preparation.
    pub definitions: Vec<DefinitionRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionRef {
    pub target: PlanTarget,
    pub source: Option<String>,
    pub content_fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Preparing,
    Running,
    Waiting,
    Stopping,
    Finishing,
    Succeeded,
    Failed,
    Cancelled,
}

impl ExecutionStatus {
    /// Finishing still occupies the active slot until descendant cleanup completes.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionOutcome {
    Succeeded { result: Content },
    Failed { error: ErrorInfo },
    Cancelled { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationView {
    pub invocation_id: InvocationId,
    pub parent_invocation_id: Option<InvocationId>,
    pub kind: InvocationKind,
    pub plan_id: PlanInstanceId,
    pub task_id: Option<TaskId>,
    pub runtime_id: Option<String>,
    pub agent_id: Option<String>,
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
    /// Actual Agent model facts; never a Session configuration override.
    pub model: Option<ModelRef>,
    pub status: InvocationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationKind {
    Plan,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationStatus {
    Preparing,
    Running,
    Waiting,
    Stopping,
    Succeeded,
    Failed,
    Cancelled,
}

impl InvocationStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRecord {
    pub request_id: RequestId,
    pub execution_id: ExecutionId,
    pub status: InputStatus,
    pub reason: Option<String>,
    pub received_at: Timestamp,
    pub processed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputStatus {
    Queued,
    Applied,
    NotApplied,
}
