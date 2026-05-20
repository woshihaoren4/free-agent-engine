mod session_plan_layer;
mod session_trait_ext;

pub use session_plan_layer::*;
pub use session_trait_ext::*;

use crate::error::Error;
use std::any::Any;
use tokio_stream::Stream;
use wd_tools::PFErr;

#[derive(Debug)]
pub struct Message {
    pub id: String,
    pub part_id: String,
    pub over: bool,
    content: Box<dyn Any + Send + 'static>,
}

pub struct Msg<T> {
    pub message: Message,
    pub content: T,
}

impl PartialEq for Message {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.part_id == other.part_id
    }
}

impl Message {
    pub fn new<Id: Into<String>>(id: Id) -> Self {
        Self {
            id: id.into(),
            part_id: "".to_string(),
            over: false,
            content: Box::new(()),
        }
    }
    pub fn set_over(mut self) -> Self {
        self.over = true;
        self
    }
    pub fn set_part_id(mut self, part_id: String) -> Self {
        self.part_id = part_id;
        self
    }
    pub fn set_raw_content(mut self, content: Box<dyn Any + Send + 'static>) -> Self {
        self.content = content;
        self
    }
    pub fn set_content<T: Any + Send + 'static>(mut self, content: T) -> Self {
        self.content = Box::new(content);
        self
    }
    pub fn try_into_inner<T>(&mut self) -> Option<T>
    where
        T: Any,
    {
        if self.content.downcast_ref::<T>().is_some() {
            let mut ctn: Box<dyn Any + Send + 'static> = Box::new(());
            std::mem::swap(&mut self.content, &mut ctn);
            let inner = ctn.downcast::<T>().unwrap();
            return Some(*inner);
        }
        None
    }
    pub fn to_msg<T>(mut self) -> Result<Msg<T>, Message>
    where
        T: Any,
    {
        let content = if let Some(s) = self.try_into_inner::<T>() {
            s
        } else {
            return Err(self);
        };
        let msg = Msg {
            message: self,
            content,
        };
        Ok(msg)
    }
}
impl<T: Any + Send + Sync + 'static> Msg<T> {
    pub fn to_message(self) -> Message {
        self.message.set_content(self.content)
    }
}

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

    /// 流式调用，返回多个消息
    async fn stream_call(
        &mut self,
        _input: Box<dyn Stream<Item = Message> + Send + Sync>,
    ) -> anyhow::Result<Vec<Message>> {
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
