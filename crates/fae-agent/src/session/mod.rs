mod text_call_stream_session;

use std::any::Any;
use tokio_stream::Stream;
use wd_tools::PFErr;
use crate::error::Error;

#[derive(Debug)]
pub struct Message {
    pub id : String,
    pub part_id: String,
    pub over: bool,
    content : Box<dyn Any + Send + 'static>,
}

pub struct Msg<T>{
    pub message: Message,
    pub content : T
}

impl PartialEq for Message {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.part_id == other.part_id
    }
}

impl Message {
    pub fn new<Id:Into<String>>(id: Id) -> Self {
        Self {
            id: id.into(),
            part_id:"".to_string(),
            over: false,
            content: Box::new(()),
        }
    }
    pub fn set_over(mut self)-> Self{
        self.over = true;self
    }
    pub fn set_part_id(mut self, part_id: String)-> Self{
        self.part_id = part_id;self
    }
    pub fn set_content<T:Any+Send+'static>(mut self, content: T)-> Self{
        self.content = Box::new(content);self
    }
    pub fn try_into_inner<T>(&mut self) -> Option<T>
    where
        T:Any,
    {
        if self.content.downcast_ref::<T>().is_some() {
            let mut ctn : Box<dyn Any + Send + 'static> = Box::new(());
            std::mem::swap(&mut self.content,&mut ctn);
            let inner = ctn.downcast::<T>().unwrap();
            return Some(*inner)
        }
        None
    }
    pub fn to_msg<T>(mut self) -> Result<Msg<T>,Message>
    where
        T:Any,
    {
        let content = if let Some(s) = self.try_into_inner::<T>(){
            s
        }else{
            return Err(self)
        };
        let msg = Msg{
            message:self,
            content,
        };
        Ok(msg)
    }
}
impl<T:Any+Send+'static> Msg<T> {
    pub fn to_message(self) -> Message {
        self.message.set_content(self.content)
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

// 一次完整的调用，返回单个消息
#[async_trait::async_trait]
pub trait SessionPingPong<In,Out>:Sync{
    async fn call(&self, _input:Msg<In>) -> anyhow::Result<Msg<Out>, Error>;
}

#[async_trait::async_trait]
impl<In:Send + 'static,Out:Send + 'static> Session for Box<dyn SessionPingPong<In,Out>>
{
    async fn call(&self, mut msg: Message) -> anyhow::Result<Message, Error> {
        let input = if let Ok(s) = msg.to_msg::<In>(){
            s
        }else{
            return Err(anyhow::anyhow!("[SessionPingPong] input message is not In").into());
        };
        let out = (**self).call(input).await?;
        Ok(out.to_message())
    }
}

// 一次完整的调用，返回流
#[async_trait::async_trait]
pub trait SessionCallStream<In,Out>:Sync{
    async fn call(&self, _input: In) ->anyhow::Result<Box<dyn Stream<Item=Out> + Send>>;
}

struct MapMessageStream<Out> {
    inner: std::pin::Pin<Box<dyn Stream<Item = Out> + Send>>,
    id: String,
}

impl<Out> Unpin for MapMessageStream<Out> {}

impl<Out: Send + 'static> Stream for MapMessageStream<Out> {
    type Item = Message;
    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx).map(|opt| {
            opt.map(|out| Message::new(self.id.clone()).set_content(out))
        })
    }
}

#[async_trait::async_trait]
impl<In:Send + 'static,Out:Send + 'static> Session for Box<dyn SessionCallStream<In,Out>>
{
    async fn call_stream(&self, mut msg: Message) -> anyhow::Result<Box<dyn Stream<Item =Message> + Send>, Error> {
        let id = msg.id.clone();
        let input = if let Some(s) = msg.try_into_inner::<In>(){
            s
        }else{
            return Err(anyhow::anyhow!("[SessionCallStream] input message is not In").into());
        };
        let out_stream = (**self).call(input).await?;
        Ok(Box::new(MapMessageStream {
            inner: Box::into_pin(out_stream),
            id,
        }))
    }
}