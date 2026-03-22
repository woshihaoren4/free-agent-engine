use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Custom error: {0}")]
    Custom(String),

    #[error("anyhow error: {0}")]
    Anyhow(#[from] anyhow::Error),
}