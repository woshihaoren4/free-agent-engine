use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use crate::{define_planning_group, AgentConfig, Context, Memory, MemoryEntry, MemoryMessageExt, NonePlan, Planning, PlanningResult, SessionCtl, SessionCtlExt, SessionMetadata, SingleAgent, SingleAgentHandle, SingleAgentPlan, TaskReq, TaskResult, TkTy};

pub trait WorkflowNode:Debug{
    fn id(&self) -> String;
    fn ty(&self) -> TkTy;
    fn build_request(&self, ctx: &Context) -> TaskReq;
    fn to(&self) -> Vec<String>;
}

#[derive(Debug)]
pub struct Workflow<S, M>{
    //工作流
    pub id: String,
    pub nodes: HashMap<String, Box<dyn WorkflowNode+Send+Sync+'static>>,
    memory: Arc<dyn MemoryMessageExt<M> + Send + 'static>,
    session_config: Arc<dyn SessionCtlExt<S> + Send + 'static>,
    agent_config: Arc<dyn AgentConfig + Send + 'static>,
}
impl<S, M> Workflow<S, M> {
    pub fn new(
        id: impl Into<String>,
        memory: Arc<dyn MemoryMessageExt<M> + Send + 'static>,
        session_config: Arc<dyn SessionCtlExt<S> + Send + 'static>,
        agent_config: Arc<dyn AgentConfig + Send + 'static>,
    ) -> Self {
        let nodes = HashMap::new();
        Self {
            id: id.into(),
            nodes,
            memory,
            session_config,
            agent_config,
        }
    }
}

#[derive(Debug)]
pub struct WorkflowPlan<S, M> {
    ctx: Context,
    memory: Arc<dyn MemoryMessageExt<M> + Send + 'static>,
    session_config: Arc<dyn SessionCtlExt<S> + Send + 'static>,
}

#[async_trait::async_trait]
impl<S,M> Planning for WorkflowPlan<S, M>
where
    S: SessionMetadata + Clone + Send + Sync + 'static,
    M:MemoryEntry + Serialize + DeserializeOwned + Clone+Send + Sync + 'static
{
    fn id(&self) -> String {
        todo!()
    }

    async fn init(&mut self) -> anyhow::Result<PlanningResult> {
        todo!()
    }

    async fn next(&mut self, event: TaskResult) -> anyhow::Result<PlanningResult> {
        todo!()
    }

    fn get_context(&self) -> Context {
        todo!()
    }
}

define_planning_group!(
    #[derive(Debug)]
    pub enum WorkflowPlanLayer<S, M>
    {
        // SessionCall(SingleAgentPlanSessionCall<M>),
        None(NonePlan),
        SessionCallStream(WorkflowPlan<S,M>),
    }
    where
    S: SessionMetadata + Clone + Send + Sync + 'static,
    M:MemoryEntry + Serialize + DeserializeOwned + Clone+Send + Sync + 'static
);

#[async_trait::async_trait]
impl<S, M> SingleAgentHandle<S, M, M, WorkflowPlanLayer<S, M>> for Workflow<S, M>
where
    S: SessionMetadata + Default + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    M: MemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    fn id(&self) -> String {
        self.id.clone()
    }

    async fn on_info(&self) -> Arc<dyn AgentConfig + Send + 'static> {
        todo!()
    }

    async fn on_memory(&self) -> Arc<dyn Memory + Send + 'static> {
        todo!()
    }

    async fn on_session_ctl(&self) -> Arc<dyn SessionCtl + Send + 'static> {
        todo!()
    }

    async fn exit(&self) {
        todo!()
    }
}