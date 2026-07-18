use std::fmt::{Debug};
use std::sync::Arc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use crate::{define_planning_group, AgentConfig, Context, Env, Memory, MemoryEntry, MemoryMessageExt, NonePlan, OutMsgOnce, Planning, PlanningResult, SessionCtl, SessionCtlExt, SessionMetadata, SingleAgent, SingleAgentHandle, SingleAgentPlan, TaskReq, TaskResult, TkTy};


// #[derive(Debug)]
// pub struct Workflow<S, M>{
//     //工作流
//     pub id: String,
//     //todo 待补充一个流程图，允许环，允许判断分支。
//     memory: Arc<dyn MemoryMessageExt<M> + Send + 'static>,
//     session_config: Arc<dyn SessionCtlExt<S> + Send + 'static>,
//     agent_config: Arc<dyn AgentConfig + Send + 'static>,
// }

// #[derive(Debug)]
// pub struct WorkflowPlan<S, M> {
//     ctx: Context,
//     memory: Arc<dyn MemoryMessageExt<M> + Send + 'static>,
//     session_config: Arc<dyn SessionCtlExt<S> + Send + 'static>,
// }

// #[async_trait::async_trait]
// impl<S,M> Planning for WorkflowPlan<S, M>
// where
//     S: SessionMetadata + Clone + Send + Sync + 'static,
//     M:MemoryEntry + Serialize + DeserializeOwned + Clone+Send + Sync + 'static
// {
//     fn id(&self) -> String {
//         todo!()
//     }

//     async fn init(&mut self) -> anyhow::Result<PlanningResult> {
//         todo!()
//     }

//     async fn next(&mut self, event: TaskResult) -> anyhow::Result<PlanningResult> {
//         todo!()
//     }

//     fn get_context(&self) -> Context {
//         todo!()
//     }
// }

// define_planning_group!(
//     #[derive(Debug)]
//     pub enum WorkflowPlanLayer<S, M>
//     {
//         // SessionCall(SingleAgentPlanSessionCall<M>),
//         None(NonePlan),
//         SessionCallStream(WorkflowPlan<S,M>),
//     }
//     where
//     S: SessionMetadata + Clone + Send + Sync + 'static,
//     M:MemoryEntry + Serialize + DeserializeOwned + Clone+Send + Sync + 'static
// );

// #[async_trait::async_trait]
// impl<S, M> SingleAgentHandle<S, M, M, WorkflowPlanLayer<S, M>> for Workflow<S, M>
// where
//     S: SessionMetadata + Default + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
//     M: MemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
// {
//     fn id(&self) -> String {
//         self.id.clone()
//     }

//     async fn on_info(&self) -> Arc<dyn AgentConfig + Send + 'static> {
//         todo!()
//     }

//     async fn on_memory(&self) -> Arc<dyn Memory + Send + 'static> {
//         todo!()
//     }

//     async fn on_session_ctl(&self) -> Arc<dyn SessionCtl + Send + 'static> {
//         todo!()
//     }

//     async fn on_session_call(&self, _env: Env, meta: &mut S, _input: M, _output: OutMsgOnce<M>) -> anyhow::Result<WorkflowPlanLayer<S, M>> {
//         todo!()
//     }

//     async fn exit(&self) {
//         todo!()
//     }
// }
