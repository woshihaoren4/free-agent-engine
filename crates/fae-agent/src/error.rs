#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("runtime is not supported")]
    RuntimeNoSupport,

    #[error(
        "accepted_task_failed: Runtime `{runtime_id}` returned NoSupport after accepting the task"
    )]
    AcceptedTaskFailed { runtime_id: String },

    #[error("context has been aborted")]
    ContextAborted,

    #[error(transparent)]
    AnyError(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
