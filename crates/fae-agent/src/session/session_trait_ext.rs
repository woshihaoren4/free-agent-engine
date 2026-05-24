use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};
use pin_project::pin_project;
use tokio_stream::{Stream, StreamExt};
use crate::{Message, Session};

// 使用 `#[pin_project]` 宏来安全地实现 Pin 投影
#[pin_project]
pub struct MessageStreamLayer<Out> {
    // `#[pin]` 告诉 pin-project 为这个字段生成投影
    #[pin]
    pub inner: Pin<Box<dyn Stream<Item = Message> + Send + Sync>>,
    _t: PhantomData<Out>,
}

impl<Out> MessageStreamLayer<Out> {
    pub fn new(inner: Box<dyn Stream<Item = Message> + Send + Sync>) -> Self {
        let inner = Box::into_pin(inner);
        Self {
            inner, // 直接赋值即可
            _t: PhantomData,
        }
    }
}

// Out 需要是 'static，因为我们的 Message 实现依赖它
impl<Out: 'static + Send> Stream for MessageStreamLayer<Out> {
    type Item = Out;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // 使用循环来处理类型不匹配的消息
        loop {
            // `self.as_mut().project()` 是 pin-project 生成的安全方法，
            // 它返回一个拥有 inner 字段的 Pin<&mut ...> 的结构体。
            let pinned_inner = self.as_mut().project().inner;

            match pinned_inner.poll_next(cx) {
                Poll::Ready(Some(mut msg)) => {
                    // 成功从内部流获取一个 Message
                    if let Some(s) = msg.try_into_inner::<Out>() {
                        // 类型匹配，返回 Ready(Some(value))
                        return Poll::Ready(Some(s));
                    } else {
                        // 类型不匹配，忽略这个消息，继续循环以获取下一个
                        wd_log::log_error_ln!("[MessageStreamLayer]ignore message, type is not Out, msg: {:?}", msg);
                        continue;
                    }
                }
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
// use crate::{Error, Message, Msg, Session};
//
// // 一次完整的调用，返回单个消息
// #[async_trait::async_trait]
// pub trait SessionPingPong<In,Out>:Sync{
//     async fn call(&self, _input:Msg<In>) -> anyhow::Result<Msg<Out>, Error>;
//     async fn abort(&self) -> anyhow::Result<()> {
//         Ok(())
//     }
// }
//
// #[async_trait::async_trait]
// impl<In:Send + 'static,Out:Send + 'static> Session for Box<dyn SessionPingPong<In,Out>>
// {
//     async fn abort(&self) -> anyhow::Result<()> {
//         (**self).abort().await
//     }
//
//     async fn call(&self, mut msg: Message) -> anyhow::Result<Message, Error> {
//         let input = if let Ok(s) = msg.to_msg::<In>(){
//             s
//         }else{
//             return Err(anyhow::anyhow!("[SessionPingPong] input message is not In").into());
//         };
//         let out = (**self).call(input).await?;
//         Ok(out.to_message())
//     }
// }

// 一次完整的调用，返回流
#[async_trait::async_trait]
pub trait SessionCallStream<In,Out>{
    async fn call_stream(&mut self, _input: In) ->anyhow::Result<Box<dyn Stream<Item=Out> + Send>>;
    async fn abort(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
#[async_trait::async_trait]
impl<In,Out> SessionCallStream<In,Out> for Box<dyn Session + Send>
where In: Send + 'static,
      Out: Send + 'static,
{
    async fn call_stream(&mut self, input: In) ->anyhow::Result<Box<dyn Stream<Item=Out> + Send>> {
        let msg = Message::default().set_content(input);
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
    In: Send + 'static,
    Out: Send + 'static,
{
    async fn call(&mut self, input: In) -> anyhow::Result<Out> {
        let msg = Message::default().set_content(input);
        let mut out_msg = (**self).call(msg).await?;
        if let Some(out) = out_msg.try_into_inner::<Out>() {
            Ok(out)
        } else {
            Err(anyhow::anyhow!("[SessionCall] output message type mismatch").into())
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

impl<In: Send + 'static> Stream for MessageInputStreamLayer<In> {
    type Item = Message;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pinned_inner = self.as_mut().project().inner;
        match pinned_inner.poll_next(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(Message::default().set_content(item))),
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
    async fn stream_call(&mut self, input: Box<dyn Stream<Item = In> + Send + Sync>) -> anyhow::Result<Out>;
    async fn abort(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl<In, Out> SessionStreamCall<In, Out> for Box<dyn Session + Send>
where
    In: Send + Sync + 'static,
    Out: Send + 'static,
{
    async fn stream_call(&mut self, input: Box<dyn Stream<Item = In> + Send + Sync>) -> anyhow::Result<Out> {
        let in_stream = Box::new(MessageInputStreamLayer::new(input));
        let mut out_msg = (**self).stream_call(in_stream).await?;
        if let Some(out) = out_msg.try_into_inner::<Out>() {
            Ok(out)
        } else {
            Err(anyhow::anyhow!("[SessionStreamCall] output message type mismatch").into())
        }
    }
    async fn abort(&mut self) -> anyhow::Result<()> {
        (**self).abort().await
    }
}

// 双向流式调用 stream
#[async_trait::async_trait]
pub trait SessionStream<In, Out> {
    async fn stream(&mut self, input: Box<dyn Stream<Item = In> + Send + Sync>) -> anyhow::Result<Box<dyn Stream<Item = Out> + Send>>;
    async fn abort(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl<In, Out> SessionStream<In, Out> for Box<dyn Session + Send>
where
    In: Send + Sync + 'static,
    Out: Send + 'static,
{
    async fn stream(&mut self, input: Box<dyn Stream<Item = In> + Send + Sync>) -> anyhow::Result<Box<dyn Stream<Item = Out> + Send>> {
        let in_stream = Box::new(MessageInputStreamLayer::new(input));
        let out_stream = (**self).stream(in_stream).await?;
        Ok(Box::new(MessageStreamLayer::new(out_stream)))
    }
    async fn abort(&mut self) -> anyhow::Result<()> {
        (**self).abort().await
    }
}

//
// pub struct MapMessageStream<Out> {
//     pub inner: std::pin::Pin<Box<dyn Stream<Item = Out> + Send>>,
//     pub id: String,
// }
//
// impl<Out> Unpin for MapMessageStream<Out> {}
//
// impl<Out: Send + 'static> Stream for MapMessageStream<Out> {
//     type Item = Message;
//     fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
//         self.inner.as_mut().poll_next(cx).map(|opt| {
//             opt.map(|out| Message::new(self.id.clone()).set_content(out))
//         })
//     }
// }
//
// #[async_trait::async_trait]
// impl<In:Send + 'static,Out:Send + 'static> Session for Box<dyn SessionCallStream<In,Out>>
// {
//     async fn abort(&self) -> anyhow::Result<()> {
//         (**self).abort().await
//     }
//
//     async fn call_stream(&self, mut msg: Message) -> anyhow::Result<Box<dyn Stream<Item =Message> + Send>, Error> {
//         let id = msg.id.clone();
//         let input = if let Some(s) = msg.try_into_inner::<In>(){
//             s
//         }else{
//             return Err(anyhow::anyhow!("[SessionCallStream] input message is not In").into());
//         };
//         let out_stream = (**self).call_stream(input).await?;
//         Ok(Box::new(MapMessageStream {
//             inner: Box::into_pin(out_stream),
//             id,
//         }))
//     }
// }
//
// //流式输入 stream_call，返回一个值
// pub struct MapInStream<In> {
//     pub inner: std::pin::Pin<Box<dyn Stream<Item = Message> + Send>>,
//     pub _marker: std::marker::PhantomData<In>,
// }
//
// impl<In> Unpin for MapInStream<In> {}
//
// impl<In: Send + 'static> Stream for MapInStream<In> {
//     type Item = In;
//     fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
//         loop {
//             match self.inner.as_mut().poll_next(cx) {
//                 std::task::Poll::Ready(Some(mut msg)) => {
//                     if let Some(s) = msg.try_into_inner::<In>() {
//                         return std::task::Poll::Ready(Some(s));
//                     }
//                 }
//                 std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
//                 std::task::Poll::Pending => return std::task::Poll::Pending,
//             }
//         }
//     }
// }
//
// #[async_trait::async_trait]
// pub trait SessionStreamCall<In, Out>: Sync {
//     async fn call(&self, _input: Box<dyn Stream<Item = In> + Send>) -> anyhow::Result<Msg<Out>, Error>;
//     async fn abort(&self) -> anyhow::Result<()> {
//         Ok(())
//     }
// }
//
// #[async_trait::async_trait]
// impl<In: Send + 'static, Out: Send + 'static> Session for Box<dyn SessionStreamCall<In, Out>> {
//     async fn abort(&self) -> anyhow::Result<()> {
//         (**self).abort().await
//     }
//
//     async fn stream_call(&self, input: Box<dyn Stream<Item = Message> + Send>) -> anyhow::Result<Vec<Message>, Error> {
//         let in_stream = Box::new(MapInStream {
//             inner: Box::into_pin(input),
//             _marker: std::marker::PhantomData,
//         });
//         let out = (**self).call(in_stream).await?;
//         Ok(vec![out.to_message()])
//     }
// }
//
// //双向流式调用 stream
// #[async_trait::async_trait]
// pub trait SessionStream<In, Out>: Sync {
//     async fn stream(&self, _input: Box<dyn Stream<Item = In> + Send>) -> anyhow::Result<Box<dyn Stream<Item = Out> + Send>, Error>;
//     async fn abort(&self) -> anyhow::Result<()> {
//         Ok(())
//     }
// }
//
// #[async_trait::async_trait]
// impl<In: Send + 'static, Out: Send + 'static> Session for Box<dyn SessionStream<In, Out>> {
//     async fn abort(&self) -> anyhow::Result<()> {
//         (**self).abort().await
//     }
//
//     async fn stream(&self, input: Box<dyn Stream<Item = Message> + Send>) -> anyhow::Result<Box<dyn Stream<Item = Message> + Send>, Error> {
//         let in_stream = Box::new(MapInStream {
//             inner: Box::into_pin(input),
//             _marker: std::marker::PhantomData,
//         });
//         let out_stream = (**self).stream(in_stream).await?;
//         Ok(Box::new(MapMessageStream {
//             inner: Box::into_pin(out_stream),
//             id: "".to_string(),
//         }))
//     }
// }
//
// /// 宏：用于为实现了任意组合（SessionPingPong / SessionCallStream / SessionStreamCall / SessionStream）的具体类型自动实现 Session trait。
// /// 注意：如果组合多个，需要按照以下顺序书写参数：PingPong, CallStream, StreamCall, Stream。
// ///
// /// 使用示例：
// /// ```rust
// /// impl_session_ext_splice!(
// ///     MyAgentStruct,
// ///     PingPong<String, String>,
// ///     CallStream<String, String>
// /// );
// /// ```
// #[macro_export]
// macro_rules! impl_session_ext_splice {
//     (
//         $type:ty
//         $(, PingPong<$pp_in:ty, $pp_out:ty>)?
//         $(, CallStream<$cs_in:ty, $cs_out:ty>)?
//         $(, StreamCall<$sc_in:ty, $sc_out:ty>)?
//         $(, Stream<$s_in:ty, $s_out:ty>)?
//     ) => {
//         #[async_trait::async_trait]
//         impl $crate::Session for $type {
//             $(
//                 async fn call(&self, msg: $crate::Message) -> anyhow::Result<$crate::Message, $crate::Error> {
//                     let input = if let Ok(s) = msg.to_msg::<$pp_in>(){
//                         s
//                     }else{
//                         return Err(anyhow::anyhow!("[SessionPingPong] input message is not In").into());
//                     };
//                     let out = $crate::SessionPingPong::call(self, input).await?;
//                     Ok(out.to_message())
//                 }
//             )?
//
//             $(
//                 async fn call_stream(&self, mut msg: $crate::Message) -> anyhow::Result<Box<dyn tokio_stream::Stream<Item =$crate::Message> + Send>, $crate::Error> {
//                     let id = msg.id.clone();
//                     let input = if let Some(s) = msg.try_into_inner::<$cs_in>(){
//                         s
//                     }else{
//                         return Err(anyhow::anyhow!("[SessionCallStream] input message is not In").into());
//                     };
//                     let out_stream = $crate::SessionCallStream::call_stream(self, input).await?;
//                     Ok(Box::new($crate::MapMessageStream {
//                         inner: Box::into_pin(out_stream),
//                         id,
//                     }))
//                 }
//             )?
//
//             $(
//                 async fn stream_call(&self, input: Box<dyn tokio_stream::Stream<Item = $crate::Message> + Send>) -> anyhow::Result<Vec<$crate::Message>, $crate::Error> {
//                     let in_stream = Box::new($crate::MapInStream::<$sc_in> {
//                         inner: Box::into_pin(input),
//                         _marker: std::marker::PhantomData,
//                     });
//                     let out = $crate::SessionStreamCall::call(self, in_stream).await?;
//                     Ok(vec![out.to_message()])
//                 }
//             )?
//
//             $(
//                 async fn stream(&self, input: Box<dyn tokio_stream::Stream<Item = $crate::Message> + Send>) -> anyhow::Result<Box<dyn tokio_stream::Stream<Item = $crate::Message> + Send>, $crate::Error> {
//                     let in_stream = Box::new($crate::MapInStream::<$s_in> {
//                         inner: Box::into_pin(input),
//                         _marker: std::marker::PhantomData,
//                     });
//                     let out_stream = $crate::SessionStream::stream(self, in_stream).await?;
//                     Ok(Box::new($crate::MapMessageStream {
//                         inner: Box::into_pin(out_stream),
//                         id: "".to_string(),
//                     }))
//                 }
//             )?
//
//             async fn abort(&self) -> anyhow::Result<()> {
//                 $(
//                     <Self as $crate::SessionPingPong<$pp_in, $pp_out>>::abort(self).await?;
//                 )?
//                 $(
//                     <Self as $crate::SessionCallStream<$cs_in, $cs_out>>::abort(self).await?;
//                 )?
//                 $(
//                     <Self as $crate::SessionStreamCall<$sc_in, $sc_out>>::abort(self).await?;
//                 )?
//                 $(
//                     <Self as $crate::SessionStream<$s_in, $s_out>>::abort(self).await?;
//                 )?
//                 Ok(())
//             }
//         }
//     };
// }
//
// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::Message;
//     use tokio_stream::StreamExt;
//
//     struct TestAgent;
//
//     #[async_trait::async_trait]
//     impl SessionPingPong<String, String> for TestAgent {
//         async fn call(&self, input: Msg<String>) -> anyhow::Result<Msg<String>, Error> {
//             let out_content = format!("Pong: {}", input.content);
//             Ok(Msg {
//                 message: Message::new("test_out"),
//                 content: out_content,
//             })
//         }
//     }
//
//     #[async_trait::async_trait]
//     impl SessionCallStream<String, String> for TestAgent {
//         async fn call_stream(&self, input: String) -> anyhow::Result<Box<dyn Stream<Item = String> + Send>> {
//             let stream = tokio_stream::iter(vec![
//                 format!("Stream1: {}", input),
//                 format!("Stream2: {}", input),
//             ]);
//             Ok(Box::new(stream))
//         }
//     }
//
//     impl_session_ext_splice!(
//         TestAgent,
//         PingPong<String, String>,
//         CallStream<String, String>
//     );
//
//     async fn test_session<S:Session>(session:S){
//         let msg = Message::new("test_in").set_content("Hello".to_string());
//         let mut out_msg = session.call(msg).await.unwrap();
//         assert_eq!(out_msg.try_into_inner::<String>().unwrap(), "Pong: Hello");
//
//         // Test CallStream (call_stream)
//         let msg2 = Message::new("test_in2").set_content("World".to_string());
//         let mut out_stream = Box::into_pin(session.call_stream(msg2).await.unwrap());
//
//         let mut msg_out1 = out_stream.next().await.unwrap();
//         assert_eq!(msg_out1.try_into_inner::<String>().unwrap(), "Stream1: World");
//
//         let mut msg_out2 = out_stream.next().await.unwrap();
//         assert_eq!(msg_out2.try_into_inner::<String>().unwrap(), "Stream2: World");
//
//         assert!(out_stream.next().await.is_none());
//     }
//
//     #[tokio::test]
//     async fn test_impl_session_ext() {
//         let session = TestAgent;
//
//         test_session(session).await;
//
//     }
// }
