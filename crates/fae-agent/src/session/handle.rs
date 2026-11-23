//! Runtime-only control contracts. No handle is part of a serializable snapshot.
use std::sync::Arc;

use crate::{
    ContextData, Ctx, ExecutionId, ExecutionOutcome, ExecutionSettings, ExecutionView, InputRecord,
    InvocationId, InvocationView, RequestId,
    protocol::{Content, SessionSummary},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub user_id: super::UserId,
    pub session_id: super::SessionId,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum SessionControlError {
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
}

pub type ControlResult<T> = Result<T, SessionControlError>;

#[derive(Debug, Clone, Default)]
pub struct ExecutionEnvironment {
    pub origin: Option<String>,
    pub timer: Option<Arc<crate::TimerExecutionBinding>>,
}

#[derive(Debug, Clone)]
pub struct QueuedInput {
    pub request_id: RequestId,
    pub content: Content,
}

#[derive(Debug, Clone)]
pub enum ExecutionReport {
    DefinitionLoaded(crate::DefinitionRef),
    ResourceRegistered {
        invocation_id: InvocationId,
        resource_id: RequestId,
    },
    ResourceReleased(RequestId),
    InitialInputAccepted(RequestId),
    MemoryLoaded(Vec<crate::UserMemory>),
    HistoryLoaded {
        partition: crate::protocol::ConversationPartition,
        messages: Vec<crate::SessionMessage>,
        offset: usize,
    },
    HistoryAdded {
        invocation_id: InvocationId,
        partition: crate::protocol::ConversationPartition,
        messages: Vec<crate::SessionMessage>,
    },
    Output {
        invocation_id: InvocationId,
        output_id: String,
        source: crate::protocol::OutputSource,
        content: Content,
    },
    InvocationRegistered(InvocationView),
    InvocationChanged(InvocationView),
    InputProcessed {
        request_id: RequestId,
        applied: bool,
        reason: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum SessionControlEvent {
    ConfigUpdated(crate::SessionConfig),
    ConversationAppended(crate::protocol::ConversationAppended),
    Output {
        execution_id: ExecutionId,
        invocation_id: InvocationId,
        output_id: String,
        source: crate::protocol::OutputSource,
        content: Content,
    },
    StateChanged(ExecutionView),
    InvocationChanged {
        execution_id: ExecutionId,
        invocation: InvocationView,
    },
    InputProcessed(InputRecord),
    Finished {
        execution_id: ExecutionId,
        outcome: ExecutionOutcome,
    },
}

/// The coordinator is the only writer of Session and Execution business facts.
#[async_trait::async_trait]
pub trait SessionControl: std::fmt::Debug + Send + Sync + 'static {
    fn key(&self) -> SessionKey;
    fn read_summary(&self) -> SessionSummary;
    fn read_context(&self) -> ContextData;
    async fn export_context(&self, source: &dyn SessionDataSource) -> ControlResult<ContextData>;
    async fn export_stored_context(&self) -> ControlResult<ContextData> {
        Err(SessionControlError::Invalid(
            "session storage source is not configured".into(),
        ))
    }
    fn reserve(&self, operation: SessionOperation) -> ControlResult<String>;
    fn release(&self, reservation: &str);
    fn commit_config(
        &self,
        reservation: &str,
        update: crate::protocol::ConfigUpdateRequest,
    ) -> ControlResult<crate::SessionConfig>;
    fn commit_clear(&self, reservation: &str) -> ControlResult<()>;
    fn archive(&self) -> ControlResult<crate::SessionConfig>;
    fn execution(&self, expected: Option<&ExecutionId>) -> ControlResult<ExecutionView>;
    fn prepare(&self, settings: ExecutionSettings) -> ControlResult<ExecutionHandle>;
    fn prepare_with_environment(
        &self,
        settings: ExecutionSettings,
        environment: ExecutionEnvironment,
    ) -> ControlResult<ExecutionHandle>;
    fn running(&self, execution_id: &ExecutionId) -> ControlResult<()>;
    fn steer(
        &self,
        execution_id: &ExecutionId,
        request_id: RequestId,
        content: Content,
    ) -> ControlResult<InputRecord>;
    fn take_inputs(&self, execution_id: &ExecutionId) -> ControlResult<Vec<QueuedInput>>;
    fn initial_input(&self, execution_id: &ExecutionId) -> ControlResult<Option<RequestId>>;
    fn stop(&self, execution_id: &ExecutionId, reason: String) -> ControlResult<bool>;
    fn report(&self, execution_id: &ExecutionId, report: ExecutionReport) -> ControlResult<()>;
    fn finishing(&self, execution_id: &ExecutionId, outcome: ExecutionOutcome)
    -> ControlResult<()>;
    /// Called only after the registered call has completed its teardown.
    fn cleaned(
        &self,
        execution_id: &ExecutionId,
        invocation_id: &InvocationId,
    ) -> ControlResult<()>;
    fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<SessionControlEvent>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOperation {
    Configure,
    Clear,
}

/// Host adapter for existing stores. Reads run outside the coordinator lock.
#[async_trait::async_trait]
pub trait SessionDataSource: Send + Sync {
    async fn read(&self, context: &ContextData) -> ControlResult<SessionStoredData>;
}

#[derive(Debug, Clone)]
pub struct SessionStoredData {
    pub history: Vec<crate::SessionMessage>,
    pub user_memories: Vec<crate::UserMemory>,
    pub project_files: Vec<crate::WorkspaceFileView>,
}

/// Releases a host-operation reservation on failure, cancellation, or normal return.
#[derive(Debug)]
pub struct SessionReservation {
    session: SessionHandle,
    token: String,
}

impl SessionReservation {
    pub fn new(session: SessionHandle, operation: SessionOperation) -> ControlResult<Self> {
        let token = session.reserve(operation)?;
        Ok(Self { session, token })
    }
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for SessionReservation {
    fn drop(&mut self) {
        self.session.release(&self.token);
    }
}

#[derive(Debug, Clone)]
pub struct SessionHandle(pub Arc<dyn SessionControl>);

impl std::ops::Deref for SessionHandle {
    type Target = dyn SessionControl;
    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionHandle {
    pub session: SessionHandle,
    pub execution_id: ExecutionId,
    pub invocation_id: InvocationId,
    pub ctx: Ctx,
}

/// Fixed identity inherited by every child scope of this execution.
#[derive(Debug, Clone)]
pub struct ExecutionBinding {
    pub session: SessionHandle,
    pub execution_id: ExecutionId,
    pub invocation_id: InvocationId,
    pub parent_invocation_id: Option<InvocationId>,
    pub definitions: Arc<tokio::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>>,
}
