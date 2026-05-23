use std::any::Any;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_stream::{Stream, StreamExt};
use wd_tools::channel::{Channel, Receiver, Sender};
use wd_tools::PFErr;

#[derive(Debug)]
pub struct Message {
    pub id: String,
    pub part_id: String,
    content: Box<dyn Any + Send + 'static>,
}

#[derive(Debug)]
pub struct Msg<T> {
    pub id: String,
    pub part_id: String,
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
            content: Box::new(()),
        }
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
            id: self.id,
            part_id: self.part_id,
            content,
        };
        Ok(msg)
    }
}

impl Default for Message {
    fn default() -> Self {
        Self::new(wd_tools::uuid::v4())
    }
}

impl<T: Any + Send + Sync + 'static> Msg<T> {
    pub fn to_message(self) -> Message {
        Message::new(self.id).set_part_id(self.part_id).set_content(self.content)
    }
}

impl<T> Msg<T> {
    pub fn new(content: T) -> Self {
        Self {
            id: wd_tools::uuid::v4(),
            part_id: "".to_string(),
            content,
        }
    }
    pub fn get_id(&self) -> &str {
        &self.id
    }
    pub fn get_part_id(&self) -> &str {
        &self.part_id
    }
    pub fn get_content(&self) -> &T {
        &self.content
    }
    pub fn set_content(&mut self, content: T) {
        self.content = content;
    }
    pub fn set_id(&mut self, id: String) {
        self.id = id;
    }
    pub fn set_part_id(&mut self, part_id: String) {
        self.part_id = part_id;
    }
}

// ------------------- Message 的流式输出 -------------------
#[derive(Debug)]
pub struct SenderMessageStream<T> {
    sender: Sender<Message>,
    inner: PhantomData<T>,
}

impl<T: Send + Sync + 'static> SenderMessageStream<T> {
    pub fn new(sender: Sender<Message>) -> Self {
        Self {
            sender,
            inner: PhantomData,
        }
    }
    pub async fn send(&self, message: Msg<T>) -> anyhow::Result<()> {
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