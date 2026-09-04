#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("runtime is not supported")]
    RuntimeNoSupport,

    #[error("context has been aborted")]
    ContextAborted,

    #[error(transparent)]
    AnyError(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
