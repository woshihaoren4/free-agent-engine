use tokio_stream::Stream;
use wd_tools::channel::{Channel, Sender};
use wd_tools::PFErr;
use crate::Error;

pub struct TextCallStreamSession<T> {
    inner: T,
    msg_channel: Channel<String>,
}
impl<T> TextCallStreamSession<T> {
    pub fn new(inner: T) -> Self {
        let msg_channel = Channel::with_cap(8);
        Self {
            inner,
            msg_channel,
        }
    }
}

#[async_trait::async_trait]
impl<T: Sync> super::SessionCallStream<String,String> for TextCallStreamSession<T> {
    async fn call(&self, _input: String) -> anyhow::Result<Box<dyn Stream<Item=String> + Send>> {
        anyhow::anyhow!("TODO").err()
    }
}