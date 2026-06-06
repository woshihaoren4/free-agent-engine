use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wd_tools::channel::Channel;

pub const MCP_STREAM_CHANNEL_SIZE: usize = 4;

#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
pub enum McpServerConfig {
    Local {
        command: String,
        description: String,
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Remote {
        description: String,
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
pub struct McpToolRequest {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
pub struct McpTools {
    pub tools:Vec<McpToolRequest>,
}

#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
pub struct McpToolRespContentText{
    pub text: String,
}

#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
pub struct McpToolRespContentImageAnnotations{
    pub audience: Vec<String>,
    pub priority: f64,
    #[serde(rename = "lastModified")]
    pub last_modified: String,
}
#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
pub struct McpToolRespContentImage{
    pub data: String,
    pub mime_type: String,
    pub annotations: Option<McpToolRespContentImageAnnotations>,
}

#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
pub struct McpToolRespContentAudio{
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
pub struct McpToolRespContentResourceLink{
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: String,
}

#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
pub struct McpToolRespContentResource{
    pub uri: String,
    pub mime_type: String,
    pub text: String,
    pub annotations: Option<McpToolRespContentImageAnnotations>,
}


#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum  McpToolRespContent{
    Text(McpToolRespContentText),
    Image(McpToolRespContentImage),
    Audio(McpToolRespContentAudio),
    ResourceLink(McpToolRespContentResourceLink),
    Resource(McpToolRespContentResource),
}

#[derive(Debug,Clone,PartialEq, Serialize,Deserialize)]
pub struct McpToolResponse {
    pub content:McpToolRespContent,
    pub isError:bool,
}

#[derive(Debug, Clone)]
pub struct McpResultChannel{
    pub channel: Channel<McpToolResponse>
}

#[derive(Debug,Clone)]
pub enum McpToolResult{
    Resp(String),
    Stream(McpResultChannel),
}

impl McpToolResult {
    pub fn with_resp(resp: String) -> Self {
        Self::Resp(resp)
    }

    pub fn with_stream() -> (Self, McpResultChannel) {
        let channel = Channel::with_cap(MCP_STREAM_CHANNEL_SIZE);
        let chan = McpResultChannel{ channel };
        (Self::Stream(chan.clone()), chan)
    }
}