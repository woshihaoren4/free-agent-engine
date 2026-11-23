use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Content = Vec<ContentBlock>;

/// Only explicitly registered public content; native values and capability handles stay internal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    Text { text: String },
    Resource { resource: ResourceRef },
    Json { value: Value },
}

/// A business resource location, separate from short-lived plugin registry handles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRef {
    pub uri: String,
    pub media_type: String,
    pub name: Option<String>,
}

/// An execution failure, separate from the integer code of a protocol response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
}
