use crate::{Message, Msg, Session};
use pin_project::pin_project;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_stream::{Stream, StreamExt};
use wd_tools::PFErr;

// 使用 `#[pin_project]` 宏来安全地实现 Pin 投影
#[pin_project]
pub struct MessageStreamLayer<Out> {
    // `#[pin]` 告诉 pin-project 为这个字段生成投影
    #[pin]
    pub inner: Pin<Box<dyn Stream<Item = Msg> + Send + Sync>>,
    _t: PhantomData<Out>,
}

impl<Out> MessageStreamLayer<Out> {
    pub fn new(inner: Box<dyn Stream<Item = Msg> + Send + Sync>) -> Self {
        let inner = Box::into_pin(inner);
        Self {
            inner, // 直接赋值即可
            _t: PhantomData,
        }
    }
}

// Out 需要是 'static，因为我们的 Message 实现依赖它
impl<Out: 'static + Send + Message> Stream for MessageStreamLayer<Out> {
    type Item = Out;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // 使用循环来处理类型不匹配的消息
        loop {
            // `self.as_mut().project()` 是 pin-project 生成的安全方法，
            // 它返回一个拥有 inner 字段的 Pin<&mut ...> 的结构体。
            let pinned_inner = self.as_mut().project().inner;

            match pinned_inner.poll_next(cx) {
                Poll::Ready(Some(mut msg)) => match msg.into_inner::<Out>() {
                    Ok(msg) => return Poll::Ready(Some(msg)),
                    Err(e) => {
                        wd_log::log_error_ln!(
                            "[MessageStreamLayer]ignore message, type is not Out, msg: {:?}",
                            e
                        );
                        continue;
                    }
                },
                Poll::Ready(None) => {
                    // 内部流已结束，所以我们的流也结束
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    // 内部流还没有准备好，我们也返回 Pending
                    return Poll::Pending;
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // (0, None) 是一个安全的选择，因为我们过滤元素，所以下界是0，上界不确定。
        // 也可以直接代理内部的 size_hint，但要注意过滤会导致实际数量变少。
        (0, self.inner.size_hint().1)
    }
}

// 一次完整的调用，返回流
#[async_trait::async_trait]
pub trait SessionCallStream<In, Out> {
    async fn call_stream(
        &mut self,
        _input: In,
    ) -> anyhow::Result<Box<dyn Stream<Item = Out> + Send>>;
    async fn abort(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
#[async_trait::async_trait]
impl<In, Out> SessionCallStream<In, Out> for Box<dyn Session + Send>
where
    In: Send + 'static + Message,
    Out: Send + 'static + Message,
{
    async fn call_stream(
        &mut self,
        input: In,
    ) -> anyhow::Result<Box<dyn Stream<Item = Out> + Send>> {
        let msg = Msg::new(input);
        let msg_stream = (**self).call_stream(msg).await?;
        Ok(Box::new(MessageStreamLayer::new(msg_stream)))
    }
    async fn abort(&mut self) -> anyhow::Result<()> {
        (**self).abort().await
    }
}

// 一次完整的调用，返回单个消息
#[async_trait::async_trait]
pub trait SessionCall<In, Out> {
    async fn call(&mut self, input: In) -> anyhow::Result<Out>;
    async fn abort(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl<In, Out> SessionCall<In, Out> for Box<dyn Session + Send>
where
    In: Send + 'static + Message,
    Out: Send + 'static + Message,
{
    async fn call(&mut self, input: In) -> anyhow::Result<Out> {
        let msg = Msg::new(input);
        let out_msg = (**self).call(msg).await?;
        match out_msg.into_inner::<Out>() {
            Ok(msg) => Ok(msg),
            Err(e) => {
                anyhow::anyhow!("[SessionCall] output message type mismatch, msg: {:?}", e).err()
            }
        }
    }
    async fn abort(&mut self) -> anyhow::Result<()> {
        (**self).abort().await
    }
}

#[pin_project]
pub struct MessageInputStreamLayer<In> {
    #[pin]
    pub inner: Pin<Box<dyn Stream<Item = In> + Send + Sync>>,
}

impl<In> MessageInputStreamLayer<In> {
    pub fn new(inner: Box<dyn Stream<Item = In> + Send + Sync>) -> Self {
        Self {
            inner: Box::into_pin(inner),
        }
    }
}

impl<In: Send + 'static + Message> Stream for MessageInputStreamLayer<In> {
    type Item = Msg;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pinned_inner = self.as_mut().project().inner;
        match pinned_inner.poll_next(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(Msg::new(item))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

// 流式输入 stream_call，返回一个值
#[async_trait::async_trait]
pub trait SessionStreamCall<In, Out> {
    async fn stream_call(
        &mut self,
        input: Box<dyn Stream<Item = In> + Send + Sync>,
    ) -> anyhow::Result<Out>;
    async fn abort(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl<In, Out> SessionStreamCall<In, Out> for Box<dyn Session + Send>
where
    In: Send + Sync + 'static + Message,
    Out: Send + 'static + Message,
{
    async fn stream_call(
        &mut self,
        input: Box<dyn Stream<Item = In> + Send + Sync>,
    ) -> anyhow::Result<Out> {
        let in_stream = Box::new(MessageInputStreamLayer::new(input));
        let out_msg = (**self).stream_call(in_stream).await?;
        match out_msg.into_inner::<Out>() {
            Ok(msg) => Ok(msg),
            Err(e) => anyhow::anyhow!(
                "[SessionStreamCall] output message type mismatch, msg: {:?}",
                e
            )
            .err(),
        }
    }
    async fn abort(&mut self) -> anyhow::Result<()> {
        (**self).abort().await
    }
}

// 双向流式调用 stream
#[async_trait::async_trait]
pub trait SessionStream<In, Out> {
    async fn stream(
        &mut self,
        input: Box<dyn Stream<Item = In> + Send + Sync>,
    ) -> anyhow::Result<Box<dyn Stream<Item = Out> + Send>>;
    async fn abort(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl<In, Out> SessionStream<In, Out> for Box<dyn Session + Send>
where
    In: Send + Sync + 'static + Message,
    Out: Send + 'static + Message,
{
    async fn stream(
        &mut self,
        input: Box<dyn Stream<Item = In> + Send + Sync>,
    ) -> anyhow::Result<Box<dyn Stream<Item = Out> + Send>> {
        let in_stream = Box::new(MessageInputStreamLayer::new(input));
        let out_stream = (**self).stream(in_stream).await?;
        Ok(Box::new(MessageStreamLayer::new(out_stream)))
    }
    async fn abort(&mut self) -> anyhow::Result<()> {
        (**self).abort().await
    }
}
