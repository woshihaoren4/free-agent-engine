use std::any::Any;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_stream::{Stream, StreamExt};
use wd_tools::channel::{Channel, Receiver, Sender};
use wd_tools::PFErr;

pub trait Message:Debug+Any{
    fn id(&self) -> &str;
}
impl Message for String {
    fn id(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug)]
pub struct Msg {
    pub inner: Box<dyn Message + Send + 'static>,
}

impl Msg {
    pub fn new<T:Message + Send + 'static>(t:T) -> Self {
        Self {
            inner: Box::new(t),
        }
    }
    pub fn get_id(&self) -> &str {
        self.inner.id()
    }
    pub fn into_inner<T: Message + Send + 'static>(self) -> Result<T, Self> {
        match self.inner.downcast::<T>() {
            Ok(boxed_t) => {
                Ok(*boxed_t)
            }
            Err(original_box) => {
                Err(Self { inner: original_box })
            }
        }
    }
}

impl Default for Msg {
    fn default() -> Self {
        Self::new(wd_tools::uuid::v4())
    }
}

// ------------------- Message 的流式输出 -------------------
#[derive(Debug)]
pub struct SenderMessageStream<T> {
    sender: Sender<Msg>,
    inner: PhantomData<T>,
}

impl<T: Send + Sync + 'static> SenderMessageStream<T> {
    pub fn new(sender: Sender<Msg>) -> Self {
        Self {
            sender,
            inner: PhantomData,
        }
    }
    pub async fn send(&self, message: T) -> anyhow::Result<()> {
        if let Err(e) = self
            .sender
            .send(message.to_message())
            .await
        {
            return Err(anyhow::anyhow!(
                "[SenderMessageStream] send message error: {:?}",
                e
            ));
        }
        Ok(())
    }
    pub fn close(&self) {
        self.sender.close();
    }
}

// ------------------- 单次 message 输出 OutMsgOnce -------------------

#[derive(Debug)]
pub struct OutMsgOnce<T> {
    channel: Channel<Msg<T>>
}
impl<T> Default for OutMsgOnce<T> {
    fn default() -> Self {
        let channel = Channel::with_cap(1);
        Self { channel }
    }
}
impl<T> Clone for OutMsgOnce<T> {
    fn clone(&self) -> Self {
        Self{ channel: self.channel.clone() }
    }
}
impl<T> OutMsgOnce<T> {
    pub async fn set(&self, msg: Msg<T>)->anyhow::Result<()> {
        if let Err(e) = self.channel.send(msg).await{
            return Err(anyhow::anyhow!("[OutMsgOnce] send message error: {:?}", e));
        }
        Ok(())
    }
    pub async fn get(&self) -> anyhow::Result<Msg<T>> {
        let msg = self.channel.recv().await?;
        Ok(msg)
    }
}

// ------------------- Message 的流式输入 -------------------

pub struct ReceiverMessageStream<T> {
    receiver: Pin<Box<dyn Stream<Item=Message> + Send + Sync>>,
    inner: PhantomData<T>,
}

impl<T: Send + Sync + 'static> ReceiverMessageStream<T> {
    pub fn new(receiver:Box<dyn Stream<Item=Message> + Send + Sync>) -> Self {
        let receiver = Box::into_pin(receiver);
        Self {
            receiver,
            inner: PhantomData,
        }
    }
    pub async fn recv(&mut self) -> anyhow::Result<Option<Msg<T>>> {
        return match self.receiver.next().await {
            Some(msg) => {
                match msg.to_msg() {
                    Ok(msg) => {
                        Ok(Some(msg))
                    },
                    Err(msg) => {
                        anyhow::anyhow!("[ReceiverMessageStream] message type not match, message=> {:?}",msg).err()
                    },
                }
            },
            None => Ok(None),
        };
    }
}

// ------------------- Message 的流式输入 到 Stream的封装 -------------------
trait IntoOpt<T> {
    fn into_opt(self) -> Option<T>;
}
impl<T> IntoOpt<T> for Option<T> {
    fn into_opt(self) -> Option<T> {
        self
    }
}
impl<T, E> IntoOpt<T> for Result<T, E> {
    fn into_opt(self) -> Option<T> {
        self.ok()
    }
}

pub struct ChannelReceiverImplStream {
    recv: Receiver<Message>,
}
impl ChannelReceiverImplStream {
    pub fn new(recv: Receiver<Message>) -> Self {
        Self { recv }
    }
}
impl Stream for ChannelReceiverImplStream {
    type Item = Message;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut fut = Box::pin(self.get_mut().recv.recv());
        match std::future::Future::poll(fut.as_mut(), cx) {
            Poll::Ready(res) => Poll::Ready(res.into_opt()),
            Poll::Pending => Poll::Pending,
        }
    }
}