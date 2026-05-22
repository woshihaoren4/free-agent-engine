mod session_trait_ext;
mod session_event_layer;

pub use session_trait_ext::*;
pub use session_event_layer::*;

use crate::error::Error;
use tokio_stream::Stream;
use wd_tools::PFErr;
use crate::define::Message;

/// 会话 trait，定义智能体与外部交互的接口
#[async_trait::async_trait]
pub trait Session: Sync {
    /// 同步调用，返回单个消息
    async fn call(&mut self, _input: Message) -> anyhow::Result<Message> {
        anyhow::Error::from(Error::NoSupport("Session.call".into())).err()
    }

    /// 调用并返回流
    async fn call_stream(
        &mut self,
        _input: Message,
    ) -> anyhow::Result<Box<dyn Stream<Item = Message> + Send + Sync>> {
        anyhow::Error::from(Error::NoSupport("Session.call_stream".into())).err()
    }

    /// 流式调用，一次返回
    async fn stream_call(
        &mut self,
        _input: Box<dyn Stream<Item = Message> + Send + Sync>,
    ) -> anyhow::Result<Message> {
        anyhow::Error::from(Error::NoSupport("Session.stream_call".into())).err()
    }

    /// 双向流式调用
    async fn stream(
        &mut self,
        _input: Box<dyn Stream<Item = Message> + Send + Sync>,
    ) -> anyhow::Result<Box<dyn Stream<Item = Message> + Send + Sync>> {
        anyhow::Error::from(Error::NoSupport("Session.stream".into())).err()
    }

    /// 终止
    async fn abort(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
