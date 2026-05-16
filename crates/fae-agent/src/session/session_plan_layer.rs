use std::marker::PhantomData;
use std::sync::Arc;
use tokio_stream::Stream;
use wd_tools::channel::{Channel, Sender};
use wd_tools::{PFErr, PFOk};
use crate::{Env, Error, Event, Message, Session, SessionInfo, Task, TaskType};
use crate::planner::{AgentPlanningExt, Planning};

pub struct SessionPlanLayer<P:Planning +Send+'static>{
    env:Env,
    session_info:SessionInfo,
    plan_builder: Arc<dyn AgentPlanningExt<P> +Send+'static>,
}
impl<P:Planning +Send+'static> SessionPlanLayer<P> {
    pub fn new(env: Env, session_info: SessionInfo, plan_builder: Arc<dyn AgentPlanningExt<P> +Send+'static>) -> Self {
        Self {
            env,
            session_info,
            plan_builder,
        }
    }
}

#[async_trait::async_trait]
impl<P:Planning+Send+'static> Session for SessionPlanLayer<P> {
    async fn call(&mut self, msg: Message) -> anyhow::Result<Message> {
        let info = std::mem::take(&mut self.session_info);
        let mut event = Event::SessionCall(info,msg);
        let plan = self.plan_builder.generate_plan(self.env.clone(),&mut event).await?;
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

    async fn call_stream(&mut self, _input: Message) -> anyhow::Result<Box<dyn Stream<Item=Message> + Send>> {
        todo!()
    }

    async fn stream_call(&mut self, _input: Box<dyn Stream<Item=Message> + Send>) -> anyhow::Result<Vec<Message>> {
        todo!()
    }

    async fn stream(&mut self, _input: Box<dyn Stream<Item=Message> + Send>) -> anyhow::Result<Box<dyn Stream<Item=Message> + Send>> {
        todo!()
    }

    async fn abort(&mut self) -> anyhow::Result<()> {
        todo!()
    }
}