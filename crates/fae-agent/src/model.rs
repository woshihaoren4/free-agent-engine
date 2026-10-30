use std::fmt::Debug;

use async_openai::types::chat::{ChatCompletionResponseStream, CreateChatCompletionResponse};

pub enum ModelResponse {
    Completed(CreateChatCompletionResponse),
    Streaming(ChatCompletionResponseStream),
}

impl Debug for ModelResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed(response) => f.debug_tuple("Completed").field(response).finish(),
            Self::Streaming(_) => f.write_str("Streaming(<chat completion stream>)"),
        }
    }
}

impl ModelResponse {
    pub(crate) fn track(&mut self, ctx: crate::Ctx, meta: crate::TaskMeta) {
        match self {
            Self::Completed(_) => {
                if ctx.set_state(crate::ContextState::succeeded(())).is_ok() {
                    let _ = ctx.append_stack(crate::ContextStackDraft::new(
                        meta,
                        crate::ContextStackPhase::Succeeded,
                    ));
                }
            }
            Self::Streaming(stream) => {
                let inner = std::mem::replace(stream, Box::pin(tokio_stream::empty()));
                let cancel_ctx = ctx.clone();
                *stream = Box::pin(TrackedModelStream {
                    inner,
                    ctx,
                    meta,
                    finished: false,
                    cancelled: Box::pin(async move { cancel_ctx.cancellation().await }),
                });
            }
        }
    }
    pub fn is_streaming(&self) -> bool {
        matches!(self, Self::Streaming(_))
    }

    pub fn into_completed(self) -> Option<CreateChatCompletionResponse> {
        match self {
            Self::Completed(response) => Some(response),
            Self::Streaming(_) => None,
        }
    }

    pub fn into_stream(self) -> Option<ChatCompletionResponseStream> {
        match self {
            Self::Completed(_) => None,
            Self::Streaming(stream) => Some(stream),
        }
    }
}

struct TrackedModelStream {
    inner: ChatCompletionResponseStream,
    ctx: crate::Ctx,
    meta: crate::TaskMeta,
    finished: bool,
    cancelled: std::pin::Pin<Box<dyn std::future::Future<Output = std::sync::Arc<str>> + Send>>,
}
impl TrackedModelStream {
    fn finish(&mut self, state: crate::ContextState, phase: crate::ContextStackPhase) {
        self.finished = true;
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
impl tokio_stream::Stream for TrackedModelStream {
    type Item = <ChatCompletionResponseStream as tokio_stream::Stream>::Item;
    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return std::task::Poll::Ready(None);
        }
        if let std::task::Poll::Ready(reason) = this.cancelled.as_mut().poll(cx) {
            this.finish(
                crate::ContextState::cancelled(reason),
                crate::ContextStackPhase::Cancelled,
            );
            return std::task::Poll::Ready(None);
        }
        let result = this.inner.as_mut().poll_next(cx);
        match &result {
            std::task::Poll::Ready(None) => this.finish(
                crate::ContextState::succeeded(()),
                crate::ContextStackPhase::Succeeded,
            ),
            std::task::Poll::Ready(Some(Err(error))) => this.finish(
                crate::ContextState::failed(error.to_string()),
                crate::ContextStackPhase::Failed,
            ),
            _ => {}
        }
        result
    }
}
impl Drop for TrackedModelStream {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(
                crate::ContextState::cancelled("model stream dropped before completion"),
                crate::ContextStackPhase::Cancelled,
            );
        }
    }
}
