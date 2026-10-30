use serde::{Deserialize, Serialize};
use serde_json::Value;
use wd_tools::PFErr;
use wd_tools::channel::Channel;

use crate::{AgentToolInvocation, Ctx};

#[derive(Debug, Clone)]
pub enum ToolInvocation {
    Agent(AgentToolInvocation),
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ToolRequest {
    pub tool_name: String,
    pub arguments: String,
    #[serde(skip)]
    invocation: Option<ToolInvocation>,
}
impl ToolRequest {
    pub fn new(tool_name: String, arguments: String) -> Self {
        Self {
            tool_name,
            arguments,
            invocation: None,
        }
    }
    pub fn with_invocation(mut self, invocation: ToolInvocation) -> Self {
        self.invocation = Some(invocation);
        self
    }
    pub fn get_arguments(&self) -> &str {
        &self.arguments
    }
    pub fn get_tool_name(&self) -> &str {
        &self.tool_name
    }
    pub fn take_invocation(&mut self) -> Option<ToolInvocation> {
        self.invocation.take()
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
    completion: Option<std::sync::Arc<ToolCompletion>>,
}
#[derive(Debug)]
struct ToolCompletion {
    ctx: Ctx,
    meta: crate::TaskMeta,
    finished: std::sync::atomic::AtomicBool,
}
impl ToolCompletion {
    fn finish(&self, state: crate::ContextState, phase: crate::ContextStackPhase) {
        if self
            .finished
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let applied = self.ctx.set_state(state).is_ok();
        let cancelled = self.ctx.state().current().is_cancelled();
        if applied || cancelled {
            let phase = if cancelled {
                crate::ContextStackPhase::Cancelled
            } else {
                phase
            };
            let _ = self
                .ctx
                .append_stack(crate::ContextStackDraft::new(self.meta.clone(), phase));
        }
    }
}
impl Drop for ToolCompletion {
    fn drop(&mut self) {
        self.finish(
            crate::ContextState::cancelled("tool response dropped before completion"),
            crate::ContextStackPhase::Cancelled,
        );
    }
}
impl Clone for ToolResponse {
    fn clone(&self) -> Self {
        Self {
            once_result: self.once_result.clone(),
            stream_chan: self.stream_chan.clone(),
            completion: self.completion.clone(),
        }
    }
}
impl ToolResponse {
    pub fn with_result(res: String) -> Self {
        Self {
            once_result: Some(res),
            stream_chan: None,
            completion: None,
        }
    }
    pub fn with_error(code: i32, msg: String) -> Self {
        let err = serde_json::json!({
            "code": code,
            "msg": msg,
        });
        let err = serde_json::to_string(&err).unwrap_or(format!("{}:{}", code, msg));
        Self {
            once_result: Some(err),
            stream_chan: None,
            completion: None,
        }
    }
    pub fn with_stream_chan() -> Self {
        let chan = Channel::with_cap(2);
        Self {
            once_result: None,
            stream_chan: Some(chan),
            completion: None,
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
        let result = if let Some(completion) = self.completion.clone() {
            tokio::select! {
                result = self.next_item() => result,
                reason = completion.ctx.cancellation() => Err(anyhow::anyhow!("{}", reason)),
            }
        } else {
            self.next_item().await
        };
        if let Some(completion) = &self.completion {
            let outcome = match &result {
                Ok(ToolRespItem::Completed(_)) => Some((
                    crate::ContextState::succeeded(()),
                    crate::ContextStackPhase::Succeeded,
                )),
                Err(error) => Some((
                    crate::ContextState::failed(error.to_string()),
                    crate::ContextStackPhase::Failed,
                )),
                _ => None,
            };
            if let Some((state, phase)) = outcome {
                completion.finish(state, phase);
            }
        }
        result
    }
    pub(crate) fn track(&mut self, ctx: Ctx, meta: crate::TaskMeta) {
        self.completion = Some(std::sync::Arc::new(ToolCompletion {
            ctx,
            meta,
            finished: std::sync::atomic::AtomicBool::new(false),
        }));
    }
    async fn next_item(&mut self) -> anyhow::Result<ToolRespItem> {
        if let Some(res) = self.once_result.take() {
            return Ok(ToolRespItem::Completed(res));
        }
        if let Some(chan) = &self.stream_chan {
            match chan.recv().await {
                Ok(item) => Ok(item),
                Err(e) => Err(anyhow::anyhow!("recv error: {:?}", e)),
            }
        } else {
            return anyhow::anyhow!("[ToolResponse] stream_chan is None").err();
        }
    }
}

#[async_trait::async_trait]
pub trait Tools: std::fmt::Debug + Send + Sync + 'static {
    fn channel(&self) -> &str;
    async fn desc(&self, ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value>;
    async fn exec(&self, ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse>;
}
