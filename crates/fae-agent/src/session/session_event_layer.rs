use crate::define::{
    ChannelReceiverImplStream, Message, OutMsgOnce, ReceiverMessageStream, SenderMessageStream,
};
use crate::{AgentEventHandle, EndPlanTaskArgs, Env, Msg, Planning, Session, SessionMetadata, SessionMD, Task, TaskType};
use std::sync::Arc;
use tokio_stream::Stream;
use wd_tools::{PFBox, PFErr, PFOk};

pub const SESSION_STREAM_CHANEL_COUNT: usize = 8;

pub struct SessionEventLayer<S, In, Out, P> {
    env: Env,
    plan_id: Option<String>,
    meta: S,
    event_handle: Arc<dyn AgentEventHandle<S, In, Out, P> + Send + 'static>,
}

impl<S: 'static, In, Out, P> SessionEventLayer<S, In, Out, P>
where
    S: SessionMetadata + Send + Sync + 'static,
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    P: Planning + Send + 'static,
{
    pub fn new(
        env: Env,
        session_info: SessionMD,
        event_handle: Arc<dyn AgentEventHandle<S, In, Out, P> + Send + 'static>,
    ) -> anyhow::Result<Self> {
        let meta = if let Ok(s) = session_info.into_inner() {
            s
        } else {
            return anyhow::anyhow!("[SessionEventLayer] session_info parse failed.").err();
        };
        Self {
            env,
            meta,
            event_handle,
            plan_id: None,
        }
        .ok()
    }
    pub async fn check_history_plan_and_abort<R: Into<String>>(
        &mut self,
        reason: R,
    ) -> anyhow::Result<()> {
        let pid = if let Some(pid) = self.plan_id.take() {
            pid
        } else {
            return Ok(());
        };
        let aid = self.event_handle.id();
        let end_plan_args = EndPlanTaskArgs::new(pid, aid, reason.into());
        let task = Task::default().set_args(end_plan_args);
        self.env.spawn(vec![task]).await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl<S, In, Out, P> Session for SessionEventLayer<S, In, Out, P>
where
    S: SessionMetadata + Send + Sync + 'static,
    In: Send + Sync + 'static + Message,
    Out: Send + Sync + 'static + Message,
    P: Planning + Send + 'static,
{
    async fn call(&mut self, input: Msg) -> anyhow::Result<Msg> {
        //先检查类型
        let msg = match input.into_inner::<In>() {
            Ok(msg) => msg,
            Err(err) => {
                return anyhow::anyhow!("[SessionEventLayer] received invalid message:{:?}", err)
                    .err();
            }
        };
        //检查是否有历史计划
        self.check_history_plan_and_abort("call").await?;
        //开始新的计划
        let output = OutMsgOnce::default();
        let plan = self
            .event_handle
            .on_session_call(self.env.clone(), &mut self.meta, msg, output.clone())
            .await?;
        let plan: Box<dyn Planning + Send + 'static> = Box::new(plan);
        self.env
            .execute(Task::new(plan.id(), self.event_handle.id(), TaskType::Plan).set_context(plan.get_context()).set_args(plan))
            .await?;
        let msg = output.get().await?;
        Ok(Msg::new(msg))
    }

    async fn call_stream(
        &mut self,
        input: Msg,
    ) -> anyhow::Result<Box<dyn Stream<Item = Msg> + Send + Sync>> {
        //先检查类型
        let msg = match input.into_inner::<In>() {
            Ok(msg) => msg,
            Err(err) => {
                return anyhow::anyhow!("[SessionEventLayer] received invalid message:{:?}", err)
                    .err();
            }
        };
        //检查是否有历史计划
        self.check_history_plan_and_abort("call").await?;
        //开始新的计划
        let (sender, receiver) = wd_tools::channel::Channel::new(SESSION_STREAM_CHANEL_COUNT);
        let output = SenderMessageStream::new(sender);
        let plan = self
            .event_handle
            .on_session_call_stream(self.env.clone(), &mut self.meta, msg, output)
            .await?;
        let plan: Box<dyn Planning + Send + 'static> = Box::new(plan);
        let task = Task::new(plan.id(), self.event_handle.id(), TaskType::Plan).set_context(plan.get_context()).set_args(plan);
        self.env.spawn(vec![task]).await?;
        let stream: Box<dyn Stream<Item = Msg> + Send + Sync> =
            ChannelReceiverImplStream::new(receiver).to_box();
        Ok(stream)
    }

    async fn stream_call(
        &mut self,
        input: Box<dyn Stream<Item = Msg> + Send + Sync>,
    ) -> anyhow::Result<Msg> {
        //先检查类型
        let input = ReceiverMessageStream::new(input);
        //检查是否有历史计划
        self.check_history_plan_and_abort("call").await?;
        //开始新的计划
        let output = OutMsgOnce::default();
        let plan = self
            .event_handle
            .on_session_stream_call(self.env.clone(), &mut self.meta, input, output.clone())
            .await?;
        let plan: Box<dyn Planning + Send + 'static> = Box::new(plan);
        self.env
            .execute(Task::new(plan.id(), self.event_handle.id(), TaskType::Plan).set_context(plan.get_context()).set_args(plan))
            .await?;
        let msg = output.get().await?;
        Ok(Msg::new(msg))
    }

    async fn stream(
        &mut self,
        input: Box<dyn Stream<Item = Msg> + Send + Sync>,
    ) -> anyhow::Result<Box<dyn Stream<Item = Msg> + Send + Sync>> {
        //先检查类型
        let input = ReceiverMessageStream::new(input);
        //检查是否有历史计划
        self.check_history_plan_and_abort("stream").await?;
        //开始新的计划
        let (sender, receiver) = wd_tools::channel::Channel::new(SESSION_STREAM_CHANEL_COUNT);
        let output = SenderMessageStream::new(sender);
        let plan = self
            .event_handle
            .on_session_stream(self.env.clone(), &mut self.meta, input, output)
            .await?;
        let plan: Box<dyn Planning + Send + 'static> = Box::new(plan);
        self.env
            .execute(Task::new(plan.id(), self.event_handle.id(), TaskType::Plan).set_context(plan.get_context()).set_context(plan.get_context()).set_args(plan))
            .await?;
        let stream: Box<dyn Stream<Item = Msg> + Send + Sync> =
            ChannelReceiverImplStream::new(receiver).to_box();
        Ok(stream)
    }
}
