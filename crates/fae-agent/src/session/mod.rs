use std::any::Any;
use tokio_stream::Stream;
use wd_tools::PFErr;
use crate::error::Error;

#[derive(Debug)]
pub struct Message {
    pub id : String,
    pub part_id: String,
    pub over: bool,
    pub content : Box<dyn Any + Send + 'static>,
}

impl PartialEq for Message {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.part_id == other.part_id
    }
}

impl Message {
    pub fn try_into_inner<T>(&mut self) -> Option<T>
    where
        T:Any + Send + 'static,
    {
        if self.content.downcast_ref::<T>().is_some() {
            let mut ctn : Box<dyn Any + Send + 'static> = Box::new(());
            std::mem::swap(&mut self.content,&mut ctn);
            let inner = ctn.downcast::<T>().unwrap();
            return Some(*inner)
        }
        None
    }
}

/// 会话 trait，定义智能体与外部交互的接口
#[async_trait::async_trait]
pub trait Session: Sync{
    /// 同步调用，返回单个消息
    async fn call(&self, _input: Message) -> anyhow::Result<Message, Error> {
        Error::NoSupport("Session.call".into()).err()
    }

    /// 调用并返回流
    async fn call_stream(&self, _input: Message) -> anyhow::Result<Box<dyn Stream<Item =Message> + Send>, Error> {
        Error::NoSupport("Session.call_stream".into()).err()
    }

    /// 流式调用，返回多个消息
    async fn stream_call(&self, _input: Box<dyn Stream<Item =Message> + Send>) -> anyhow::Result<Vec<Message>, Error> {
        Error::NoSupport("Session.stream_call".into()).err()
    }

    /// 双向流式调用
    async fn stream(&self, _input: Box<dyn Stream<Item =Message> + Send>) -> anyhow::Result<Box<dyn Stream<Item =Message> + Send>, Error> {
        Error::NoSupport("Session.stream".into()).err()
    }

    /// 终止
    async fn abort(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait SessionPingPong<In,Out>:Sync{
    async fn call(&self, _input: In) -> anyhow::Result<Out, Error>;
}