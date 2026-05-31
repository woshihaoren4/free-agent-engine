use crate::{Command, EnvEvent, Msg, SessionMD};
use tokio_stream::Stream;
use wd_tools::channel::Sender;

/// 事件类型，表示系统中发生的各种事件
#[derive(Default)]
pub enum Event {
    /// 无事件
    #[default]
    None,
    /// session事件
    SessionCall(SessionMD, Msg),
    SessionCallStream(SessionMD, Msg, Sender<Msg>),
    SessionStreamCall(
        SessionMD,
        Box<dyn Stream<Item = Msg> + Send + Sync + 'static>,
    ),
    SessionStream(
        SessionMD,
        Box<dyn Stream<Item = Msg> + Send + Sync + 'static>,
        Sender<Msg>,
    ),
    /// 环境事件
    EnvEvent(EnvEvent),
    /// 命令
    Command(Command),
}
