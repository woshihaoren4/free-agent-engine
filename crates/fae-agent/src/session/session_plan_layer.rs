use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio_stream::Stream;
use wd_tools::channel::{Channel, Receiver, Sender};
use wd_tools::{PFBox, PFErr, PFOk};
use wd_tools::time::format::Item;
use crate::{Env, Error, Event, Message, Session, SessionInfo, Task, TaskType};
use crate::planner::{AgentPlanningExt, EndPlanTaskArgs, Planning};

const SESSION_STREAM_CHANEL_COUNT: usize = 8;

trait IntoOpt<T> {
    fn into_opt(self) -> Option<T>;
}
impl<T> IntoOpt<T> for Option<T> {
    fn into_opt(self) -> Option<T> { self }
}
impl<T, E> IntoOpt<T> for Result<T, E> {
    fn into_opt(self) -> Option<T> { self.ok() }
}

pub struct ChannelReceiverImplStream{
    recv: Receiver<Message>
}
impl ChannelReceiverImplStream{
    pub fn new(recv: Receiver<Message>) -> Self{
        Self{recv}
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

pub struct SessionPlanLayer<P:Planning +Send+'static>{
    env:Env,
    session_id: String,
    plan_id: String,
    session_info:SessionInfo,
    plan_builder: Arc<dyn AgentPlanningExt<P> +Send+'static>,
}
impl<P:Planning +Send+'static> SessionPlanLayer<P> {
    pub fn new(env: Env, session_info: SessionInfo, plan_builder: Arc<dyn AgentPlanningExt<P> +Send+'static>) -> Self {
        let session_id = session_info.get_session_id().to_string();
        Self {
            env,
            session_id,
            session_info,
            plan_builder,
            plan_id: String::new(),
        }
    }
}

#[async_trait::async_trait]
impl<P:Planning+Send+'static> Session for SessionPlanLayer<P> {
    async fn call(&mut self, msg: Message) -> anyhow::Result<Message> {
        let info = std::mem::take(&mut self.session_info);
        let mut event = Event::SessionCall(info,msg);
        let plan = self.plan_builder.generate_plan(self.env.clone(),&mut event).await?;
        self.plan_id = plan.id().to_string();
        let task = Task::new(plan.id(), self.plan_builder.id(),TaskType::Plan);
        let result = self.env.execute(task).await?;
        if result.is_error() {
            return Err(anyhow::anyhow!("[SessionPlanLayer::plan::task] error, result=>{:?}",result).into());
        }
        if let Some(s) = result.data {
            Message::new(result.task_id).set_over().set_raw_content(s).ok()
        }else{
            Err(anyhow::anyhow!("[SessionPlanLayer::plan::task] result is nil").into())
        }
    }

    async fn call_stream(&mut self, input: Message) -> anyhow::Result<Box<dyn Stream<Item=Message> + Send + Sync>> {
        let info = std::mem::take(&mut self.session_info);
        let (sender,receiver) = wd_tools::channel::Channel::new(SESSION_STREAM_CHANEL_COUNT);

        let mut event = Event::SessionCallStream(info, input, sender);
        let plan = self.plan_builder.generate_plan(self.env.clone(), &mut event).await?;
        self.plan_id = plan.id().to_string();
        let task = Task::new(plan.id(), self.plan_builder.id(), TaskType::Plan);
        self.env.spawn(vec![task]).await?;
        let stream:Box<dyn Stream<Item=Message>+Send+Sync>  = ChannelReceiverImplStream::new(receiver).to_box();
        Ok(stream)
    }

    async fn stream_call(&mut self, input: Box<dyn Stream<Item=Message> + Send + Sync>) -> anyhow::Result<Vec<Message>> {
        let info = std::mem::take(&mut self.session_info);
        let mut event = Event::SessionStreamCall(info, input);
        let plan = self.plan_builder.generate_plan(self.env.clone(), &mut event).await?;
        self.plan_id = plan.id().to_string();
        let task = Task::new(plan.id(), self.plan_builder.id(), TaskType::Plan);
        let mut result = self.env.execute(task).await?;
        if result.is_error() {
            return Err(anyhow::anyhow!("[SessionPlanLayer::plan::task] error, result=>{:?}", result).into());
        }
        if let Some(s) = result.into_inner::<Vec<Message>>() {
            Ok(s)
        } else {
            Err(anyhow::anyhow!("[SessionPlanLayer::plan::task] result is nil").into())
        }
    }

    async fn stream(&mut self, input: Box<dyn Stream<Item=Message> + Send + Sync>) -> anyhow::Result<Box<dyn Stream<Item=Message> + Send + Sync>> {
        let info = std::mem::take(&mut self.session_info);
        let (send,recv) = wd_tools::channel::Channel::new(SESSION_STREAM_CHANEL_COUNT);
        let mut event = Event::SessionStream(info, input,send);
        let plan = self.plan_builder.generate_plan(self.env.clone(), &mut event).await?;
        self.plan_id = plan.id().to_string();
        let task = Task::new(plan.id(), self.plan_builder.id(), TaskType::Plan);
        self.env.spawn(vec![task]).await?;
        let stream:Box<dyn Stream<Item=Message> + Send + Sync> = ChannelReceiverImplStream::new(recv).to_box();
        Ok(stream)
    }

    async fn abort(&mut self) -> anyhow::Result<()> {
        let args = EndPlanTaskArgs::new(self.plan_id.clone(), self.plan_builder.id());
        let task = Task::new(self.plan_id.as_str(), self.plan_builder.id(), TaskType::EndPlan).set_args(args);
        self.env.spawn(vec![task]).await?;
        Ok(())
    } 
}