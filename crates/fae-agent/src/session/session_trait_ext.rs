// use tokio_stream::Stream;
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
//
// // 一次完整的调用，返回流
// #[async_trait::async_trait]
// pub trait SessionCallStream<In,Out>:Sync{
//     async fn call_stream(&self, _input: In) ->anyhow::Result<Box<dyn Stream<Item=Out> + Send>>;
//     async fn abort(&self) -> anyhow::Result<()> {
//         Ok(())
//     }
// }
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
