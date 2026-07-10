use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wd_tools::PFErr;
use wd_tools::channel::Channel;

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
    // true: 完成, false: 未完成，当completed时仍然为false，则表示任务失败
    status: Arc<AtomicBool>,
}
impl Clone for ToolResponse {
    fn clone(&self) -> Self {
        Self {
            once_result: self.once_result.clone(),
            stream_chan: self.stream_chan.clone(),
            status: self.status.clone(),
        }
    }
}
impl ToolResponse {
    pub fn with_result(res: String) -> Self {
        Self {
            once_result: Some(res),
            stream_chan: None,
            status: Arc::new(AtomicBool::new(true)),
        }
    }
    pub fn with_stream_chan() -> Self {
        let chan = Channel::with_cap(2);
        Self {
            once_result: None,
            stream_chan: Some(chan),
            status: Arc::new(AtomicBool::new(false)),
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
    pub async fn success_completed_push(&self, item: String) -> anyhow::Result<()> {
        self.set_status_to_success();
        self.completed_push(item).await?;
        Ok(())
    }
    pub async fn error_completed_push(&self, item: String) -> anyhow::Result<()> {
        self.completed_push(item).await?;
        Ok(())
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
            anyhow::anyhow!("[ToolResponse] stream_chan is None").err()
        }
    }
    pub fn is_streaming(&self) -> bool {
        self.stream_chan.is_some()
    }
    pub fn get_status(&self) -> bool {
        self.status.load(Ordering::Relaxed)
    }
    pub fn set_status_to_success(&self) {
        self.status.store(true, Ordering::Relaxed);
    }
}
