use serde::{Deserialize, Serialize};

use crate::{
    ChannelBinding, ExecutionId, ExecutionOptions, ExecutionOutcome, ExecutionStatus,
    ExecutionView, InputRecord, InvocationId, InvocationView, PlanTarget, RequestId, SessionConfig,
    SessionMessage, Timestamp, UserInfo,
    plugin::{PluginDeclaration, PluginMount},
};

use super::Content;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloRequest {
    pub versions: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineInstance {
    pub id: String,
    pub pid: u32,
    pub app_version: String,
    pub started_at: Timestamp,
    pub address: String,
    pub port: u16,
    pub executable: String,
    pub home: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayCapabilities {
    pub max_events: usize,
    pub max_bytes: usize,
    pub persistent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestRetention {
    pub max_entries: usize,
    pub user_scoped: bool,
    pub persistent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCapabilities {
    pub max_history_limit: usize,
    pub replay: ReplayCapabilities,
    pub requests: RequestRetention,
    pub execution_restore: bool,
    pub full_config_restore: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloResult {
    pub version: u16,
    pub instance: EngineInstance,
    pub capabilities: EngineCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineStatus {
    pub sessions: usize,
    pub active_executions: usize,
    pub shutting_down: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitializeRequest {
    pub agent_id: String,
    pub model: String,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitializeResult {
    pub config_path: String,
    pub skill_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownResult {
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginShowRequest {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInstallRequest {
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginUninstallRequest {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginMutationResult {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginHost {
    Online,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRuntimeStatus {
    pub healthy: bool,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    pub active_calls: usize,
    pub active_streams: usize,
    pub pending_requests: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInstance {
    pub instance_id: String,
    pub mount: PluginMount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginStatus {
    pub id: String,
    pub order: usize,
    pub installed: bool,
    pub enabled: bool,
    pub source: String,
    pub declaration: Option<PluginDeclaration>,
    pub loaded: bool,
    pub host: PluginHost,
    pub runtime: Option<PluginRuntimeStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_order: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instances: Option<Vec<PluginInstance>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginShowResult {
    pub plugins: Vec<PluginStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCreateRequest {
    pub name: String,
    pub target: PlanTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default)]
    pub options: ExecutionOptions,
    #[serde(default)]
    pub channels: Vec<ChannelBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySummary {
    pub history_loaded: bool,
    pub history_offset: usize,
    pub history_count: usize,
    pub user_memories_loaded: bool,
    pub user_memory_count: usize,
    pub project_files_loaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSummary {
    pub execution_id: ExecutionId,
    pub status: ExecutionStatus,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSummary {
    pub memory: MemorySummary,
    pub latest_execution: Option<ExecutionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSummary {
    pub user_info: UserInfo,
    pub session_config: SessionConfig,
    pub context: ContextSummary,
    pub active_execution: Option<ExecutionId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigUpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PlanTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ExecutionOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationListRequest {
    pub offset: usize,
    pub limit: usize,
}

/// The actual existing history partition, including child Agent partitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationPartition {
    pub target_id: String,
    pub user_id: String,
    pub session_id: crate::SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationPage {
    pub partition: ConversationPartition,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub messages: Vec<SessionMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationClearResult {
    pub cleared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionStartRequest {
    pub input: Content,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PlanTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ExecutionOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionAccepted {
    pub execution_id: ExecutionId,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSteerRequest {
    pub execution_id: ExecutionId,
    pub input: Content,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputAccepted {
    pub execution_id: ExecutionId,
    pub request_id: RequestId,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionStopRequest {
    pub execution_id: ExecutionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionStopResult {
    pub execution_id: ExecutionId,
    pub accepted: bool,
    pub status: ExecutionStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionGetRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<ExecutionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_request_id: Option<RequestId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceCertainty {
    Known,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceFact {
    pub request_id: RequestId,
    pub certainty: AcceptanceCertainty,
    pub execution_id: Option<ExecutionId>,
    pub input: Option<InputRecord>,
    pub outcome: Option<ExecutionOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionGetResult {
    pub execution: Option<ExecutionView>,
    pub acceptance: Option<AcceptanceFact>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeResult {
    pub subscription_id: String,
    pub stream_id: String,
    pub snapshot_seq: u64,
    pub snapshot: SessionSummary,
    pub replay: ReplayCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsubscribeRequest {
    pub subscription_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsubscribeResult {
    pub subscription_id: String,
    pub unsubscribed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigUpdated {
    pub config: SessionConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationAppended {
    pub execution_id: ExecutionId,
    pub invocation_id: InvocationId,
    pub partition: ConversationPartition,
    pub messages: Vec<SessionMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionStateChanged {
    pub execution_id: ExecutionId,
    pub status: ExecutionStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputApplied {
    pub execution_id: ExecutionId,
    pub request_id: RequestId,
    pub applied_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputNotApplied {
    pub execution_id: ExecutionId,
    pub request_id: RequestId,
    pub reason: String,
    pub processed_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationChanged {
    pub execution_id: ExecutionId,
    pub invocation: InvocationView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputDelta {
    pub execution_id: ExecutionId,
    pub invocation_id: InvocationId,
    pub output_id: String,
    pub source: OutputSource,
    pub content: Content,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputSource {
    Model,
    Tool {
        /// Callable name shown to the model, including MCP and Agent tools.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
    },
    Plugin {
        plugin_id: String,
    },
    /// Final answer produced by complete_query, rather than a plugin notification.
    PluginResult {
        plugin_id: String,
    },
    Runtime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionFinished {
    pub execution_id: ExecutionId,
    pub outcome: ExecutionOutcome,
}
