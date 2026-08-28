use std::{fmt::Debug, path::PathBuf};

#[async_trait::async_trait]
pub trait Session<In, Out>: Debug + Send + Sync + 'static {
    async fn call(&self, input: In) -> anyhow::Result<()>;
    async fn answer(&self) -> anyhow::Result<Option<Out>>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionMessage {
    pub role: SessionMessageRole,
    pub content: String,
}

impl SessionMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: SessionMessageRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: SessionMessageRole::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionQuery {
    pub user: String,
    pub session_id: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

impl SessionQuery {
    pub fn new(user: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            session_id: session_id.into(),
            limit: None,
            offset: None,
        }
    }

    pub fn with_page(
        user: impl Into<String>,
        session_id: impl Into<String>,
        limit: impl Into<Option<usize>>,
        offset: impl Into<Option<usize>>,
    ) -> Self {
        Self {
            user: user.into(),
            session_id: session_id.into(),
            limit: limit.into(),
            offset: offset.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionRequest {
    Add {
        user: String,
        session_id: String,
        messages: Vec<SessionMessage>,
    },
    Delete {
        user: String,
        session_id: String,
    },
    Query {
        user: String,
        session_id: String,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionResponse {
    Added {
        path: PathBuf,
        added: usize,
    },
    Deleted {
        path: PathBuf,
        existed: bool,
    },
    History {
        path: PathBuf,
        messages: Vec<SessionMessage>,
    },
}
