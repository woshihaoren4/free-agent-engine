use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ToolRequest {
    pub tool_name: String,
    pub arguments: String,
}
impl ToolRequest {
    pub fn new(tool_name: String, arguments: String) -> Self {
        Self {
            tool_name,
            arguments,
        }
    }
    pub fn get_arguments(&self) -> &str {
        &self.arguments
    }
    pub fn get_tool_name(&self) -> &str {
        &self.tool_name
    }
}
