use thiserror::Error;

pub const TASK_ERROR_CODE_UNKNOWN: i32 = 999001001;


#[derive(Error, Debug)]
pub enum Error {
    #[error("NoSupport error: {0}")]
    NoSupport(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Custom error: {0}")]
    Custom(String),

    #[error("anyhow error: {0}")]
    Anyhow(#[from] anyhow::Error),
}
