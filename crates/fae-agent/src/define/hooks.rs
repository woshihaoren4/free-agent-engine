use crate::{Message, SenderMessageStream};
use std::any::Any;
use std::fmt::Debug;

#[derive(Debug, Default)]
pub struct Hook;
#[derive(Debug, Default)]
pub struct Trigger;

#[derive(Debug, Default,Clone)]
pub struct AgentCallSessionOver{
    pub have_sub_task:bool
}
impl AgentCallSessionOver {
    pub fn set_have_sub_task(&mut self) {
        self.have_sub_task = true;
    }
    pub fn get_have_sub_task(&self) -> bool {
        self.have_sub_task
    }
}

impl Hook {
    pub fn agent_call_session_over<F>(
        agent_id: &str,
        session_id: &str,
        handle: impl FnOnce(&mut wd_event::Context, &mut AgentCallSessionOver) -> F
        + Send
        + Sync
        + 'static,
    ) where
        F: for<'a> Future<Output = anyhow::Result<()>> + Send,
    {
        let key = format!("agent_call_session_over_{}:{}", agent_id, session_id);
        wd_event::register_event_once(key, handle)
    }
}
impl Trigger {
    pub async fn agent_call_session_over(
        agent_id: &str,
        session_id: &str,
        over_info: AgentCallSessionOver,
    ) -> anyhow::Result<AgentCallSessionOver>{
        let info = over_info.clone();
        let key = format!("agent_call_session_over_{}:{}", agent_id, session_id);
        let mut ctx = wd_event::invoke(key, over_info).await?;
        if let Some(out) = ctx.try_into_inner::<AgentCallSessionOver>() {
            Ok(out)
        } else {
            Ok(info)
        }
    }
}
