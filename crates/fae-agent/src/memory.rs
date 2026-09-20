use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserMemoryCategory {
    UserAttribute,
    Preference,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserMemoryConfidence {
    UserStated,
    UserConfirmed,
    SystemInferred,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMemory {
    pub id: u64,
    pub category: UserMemoryCategory,
    pub content: String,
    pub confidence: UserMemoryConfidence,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMemoryQuery {
    pub user_id: String,
}

impl UserMemoryQuery {
    pub fn new(user_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserMemoryRequest {
    Query {
        user_id: String,
    },
    Update {
        user_id: String,
        #[serde(default)]
        id: Option<u64>,
        category: UserMemoryCategory,
        content: String,
        confidence: UserMemoryConfidence,
    },
    Delete {
        user_id: String,
        id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserMemoryResponse {
    Memories {
        path: PathBuf,
        memories: Vec<UserMemory>,
    },
    Updated {
        path: PathBuf,
        memory: UserMemory,
        created: bool,
    },
    Deleted {
        path: PathBuf,
        memory: UserMemory,
    },
}
