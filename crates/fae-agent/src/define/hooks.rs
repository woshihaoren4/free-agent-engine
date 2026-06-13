use crate::{Message, SenderMessageStream};
use std::any::Any;
use std::fmt::Debug;

#[derive(Debug, Default)]
pub struct Hook;
#[derive(Debug, Default)]
pub struct Trigger;

impl Hook {
    pub fn agent_call_session_over<F>(
        agent_id: &str,
        session_id: &str,
        handle: impl FnOnce(&mut wd_event::Context, Box<dyn Any + Sync + Send + 'static>) -> F
        + Send
        + Sync
        + 'static,
    ) where
        F: Future<Output = anyhow::Result<()>> + Send,
    {
        let key = format!("agent_call_session_over_{}:{}", agent_id, session_id);
        wd_event::register_event_once(key, handle)
    }
}
impl Trigger {
    pub fn agent_call_session_over<M: Message + Sync + Send + 'static>(
        agent_id: &str,
        session_id: &str,
        output: SenderMessageStream<M>,
    ) {
        let key = format!("agent_call_session_over_{}:{}", agent_id, session_id);
        let input: Box<dyn Any + Sync + Send + 'static> = Box::new(output);
        wd_event::launch_fn(key, input, |mut ctx| {
            if let Some(out) = ctx.try_into_inner::<Box<dyn Any + Sync + Send + 'static>>() {
                if let Ok(out) = out.downcast::<SenderMessageStream<M>>() {
                    out.close();
                }
            }
        });
    }
}
