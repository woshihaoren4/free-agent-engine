use tokio_stream::Stream;
use wd_tools::channel::Sender;
use crate::define::{Message, SenderMessageStream};
use crate::{Command, EnvEvent, SessionMetadata};

/// 事件类型，表示系统中发生的各种事件
#[derive(Default)]
pub enum Event {
    /// 无事件
    #[default]
    None,
    /// session事件
    SessionCall(SessionMetadata, Message),
    SessionCallStream(SessionMetadata, Message, Sender<Message>),
    SessionStreamCall(
        SessionMetadata,
        Box<dyn Stream<Item = Message> + Send + Sync + 'static>,
    ),
    SessionStream(
        SessionMetadata,
        Box<dyn Stream<Item = Message> + Send + Sync + 'static>,
        Sender<Message>,
    ),
    /// 环境事件
    EnvEvent(EnvEvent),
    /// 命令
    Command(Command),
}



impl Event {
    pub fn sender_message_to_stream_t<M:Send + Sync + 'static>(sender: Sender<Message>) -> SenderMessageStream<M> {
        SenderMessageStream::<M>::new(sender)
    }
}