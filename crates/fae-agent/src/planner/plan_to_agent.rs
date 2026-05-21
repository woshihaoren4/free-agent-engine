use crate::planner::{AgentPlanningExt, Planning};
use crate::{
    Agent, Command, Env, EnvEvent, Event, Session, SessionInfo, SessionPlanLayer, Task, TaskType,
};
use std::marker::PhantomData;
use std::sync::Arc;

pub struct PlanToAgent<F, P> {
    f: Arc<F>,
    p: PhantomData<P>,
}

impl<F, P> PlanToAgent<F, P> {
    pub fn new(f: Arc<F>) -> Self {
        Self { f, p: PhantomData }
    }
}

#[async_trait::async_trait]
impl<F, P> Agent for PlanToAgent<F, P>
where
    P: Planning + Send + 'static,
    F: AgentPlanningExt<P> + Send + 'static,
{
    fn id(&self) -> String {
        self.f.id()
    }

    async fn on_env(&self, env: Env, event: EnvEvent) -> anyhow::Result<()> {
        let event = Event::EnvEvent(event);
        let plan = self.f.generate_plan(env.clone(), event).await?;
        let plan: Box<dyn Planning + Send + 'static> = Box::new(plan);
        let plan_id = plan.id();
        let agent_id = self.f.id();
        let task = Task::new(plan_id, agent_id, TaskType::Plan).set_args(Box::new(plan));
        env.spawn(vec![task]).await?;
        Ok(())
    }

    async fn on_session(
        &self,
        env: Env,
        meta: SessionInfo,
    ) -> anyhow::Result<Box<dyn Session + Send + 'static>> {
        let session = SessionPlanLayer::new(env, meta, self.f.clone());
        Ok(Box::new(session))
    }

    async fn on_command(&self, env: Env, cmd: Command) -> anyhow::Result<()> {
        let event = Event::Command(cmd);
        let plan = self.f.generate_plan(env.clone(), event).await?;
        let plan: Box<dyn Planning + Send + 'static> = Box::new(plan);
        let plan_id = plan.id();
        let agent_id = self.f.id();
        let task = Task::new(plan_id, agent_id, TaskType::Plan).set_args(Box::new(plan));
        env.spawn(vec![task]).await?;
        Ok(())
    }

    async fn exit(&self) {
        self.f.exit().await
    }
}
