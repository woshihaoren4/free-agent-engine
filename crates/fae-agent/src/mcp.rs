use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    Local {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Remote {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpQuery {
    pub server: String,
}

impl McpQuery {
    pub fn new(server: impl Into<String>) -> Self {
        Self {
            server: server.into(),
        }
    }
}

impl From<String> for McpQuery {
    fn from(server: String) -> Self {
        Self::new(server)
    }
}

impl From<&str> for McpQuery {
    fn from(server: &str) -> Self {
        Self::new(server)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub server: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: Value,
}

impl McpToolInfo {
    pub fn model_name(&self) -> String {
        format!("{}__{}", self.server, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpRequest {
    pub server: String,
    pub tool_name: String,
    pub arguments: String,
}

impl McpRequest {
    pub fn new(
        server: impl Into<String>,
        tool_name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            server: server.into(),
            tool_name: tool_name.into(),
            arguments: arguments.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpResponse {
    pub output: String,
}
