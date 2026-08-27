use serde::{Deserialize, Serialize};
use serde_json::Value;
use wd_tools::PFErr;
use wd_tools::channel::Channel;

use crate::Ctx;

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


#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolRespItem {
    Streaming(String),
    Completed(String),
}

#[derive(Debug)]
pub struct ToolResponse {
    once_result: Option<String>,
    stream_chan: Option<Channel<ToolRespItem>>,
}
impl Clone for ToolResponse {
    fn clone(&self) -> Self {
        Self {
            once_result: self.once_result.clone(),
            stream_chan: self.stream_chan.clone(),
        }
    }
}
impl ToolResponse {
    pub fn with_result(res: String) -> Self {
        Self {
            once_result: Some(res),
            stream_chan: None,
        }
    }
    pub fn with_error(code:i32, msg: String) -> Self {
        let err = serde_json::json!({
            "code": code,
            "msg": msg,
        });
        let err = serde_json::to_string(&err).unwrap_or(format!("{}:{}",code,msg));
        Self {
            once_result: Some(err),
            stream_chan: None,
        }
    }
    pub fn with_stream_chan() -> Self {
        let chan = Channel::with_cap(2);
        Self {
            once_result: None,
            stream_chan: Some(chan),
        }
    }
    pub async fn streaming_push(&self, item: String) -> anyhow::Result<()> {
        if let Some(chan) = &self.stream_chan {
            chan.send(ToolRespItem::Streaming(item)).await?;
            Ok(())
        } else {
            anyhow::anyhow!("stream_chan is None").err()
        }
    }
    pub async fn completed_push(&self, item: String) -> anyhow::Result<()> {
        if let Some(chan) = &self.stream_chan {
            chan.send(ToolRespItem::Completed(item)).await?;
            chan.close();
            Ok(())
        } else {
            anyhow::anyhow!("stream_chan is None").err()
        }
    }
    pub fn completed(&self) {
        if let Some(chan) = &self.stream_chan {
            chan.close();
        }
    }
    pub async fn next(&mut self) -> anyhow::Result<ToolRespItem> {
        if let Some(res) = self.once_result.take() {
            return Ok(ToolRespItem::Completed(res));
        }
        if let Some(chan) = &self.stream_chan {
            match chan.recv().await {
                Ok(item) => Ok(item),
                Err(e) => Err(anyhow::anyhow!("recv error: {:?}", e)),
            }
        } else {
            return anyhow::anyhow!("[ToolResponse] stream_chan is None").err()
        }
    }
}

#[async_trait::async_trait]
pub trait Tools: std::fmt::Debug + Send + Sync + 'static {
    fn channel(&self) -> &str;
    async fn desc(&self, ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value>;
    async fn exec(&self, ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse>;
}
