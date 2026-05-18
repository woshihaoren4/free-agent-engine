use std::sync::Arc;
use serde::{Deserialize, Serialize};
use wd_tools::channel::Sender;
use crate::memory::Memory;
use crate::{define_planning_group, Command, Env, EnvEvent, Event, Message, PlanningResult, SessionInfo, SessionMetaManager, Task, TaskResult};
use crate::planner::{AgentPlanningExt, Planning, PlanningItem};
use crate::session::Session;

#[derive(Default, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SingleAgentSessionConfig {
    /// 基础路径，用于存储代理的配置文件和状态文件
    pub base_path: String,
    /// 模型名称,如："gpt-3.5-turbo"
    pub model_name: String,
    /// 模型渠道,如："OpenAI_API"
    pub model_channel: String,
    /// 模型 API 密钥
    pub model_api_key: String,
    /// 模型 API 基础 URL
    pub model_api_base_url: String,
}

pub struct SingleAgent<M>{
    agent_id: String,
    memory: Arc<dyn Memory<M>+Send+'static>,
    session_manager: Arc<dyn SessionMetaManager<SingleAgentSessionConfig>+Send+'static>,
}

pub struct SingleAgentPlanSessionCall<M>{
    id: String,
    session_info: SessionInfo,
    input: String,
    memory: Arc<dyn Memory<M>+Send+Sync+'static>,
}

#[async_trait::async_trait]
impl<M:Send+Sync+'static> Planning for SingleAgentPlanSessionCall<M>{
    fn id(&self) -> String {
        todo!()
    }

    async fn start(&mut self) -> anyhow::Result<PlanningResult> {
        Ok(PlanningResult::End(None))
    }

    async fn next(&mut self, event: TaskResult) -> anyhow::Result<PlanningResult> {
        todo!()
    }
}

pub struct SingleAgentPlanSessionCallStream<M>{
    id: String,
    session_info: SessionInfo,
    input: String,
    output: Sender<M>,
    memory: Arc<dyn Memory<M>+Send+Sync+'static>,
}

#[async_trait::async_trait]
impl<M:Send+Sync+'static> Planning for SingleAgentPlanSessionCallStream<M>{
    fn id(&self) -> String {
        todo!()
    }
    async fn start(&mut self) -> anyhow::Result<PlanningResult> {
        Ok(PlanningResult::End(None))
    }

    async fn next(&mut self, event: TaskResult) -> anyhow::Result<PlanningResult> {
        todo!()
    }
}

define_planning_group!(
    pub enum SingleAgentPlan<M> {
        SessionCall(SingleAgentPlanSessionCall<M>),
        SessionCallStream(SingleAgentPlanSessionCallStream<M>),
    }
);

#[async_trait::async_trait]
impl<M:Send+Sync+'static> AgentPlanningExt<SingleAgentPlan<M>> for SingleAgent<M> {
    fn id(&self) -> String {
        todo!()
    }

    async fn generate_plan(&self, env: Env, event: Event) -> anyhow::Result<SingleAgentPlan<M>> {
        todo!()
    }

    async fn exit(&self) -> anyhow::Result<()> {
        todo!()
    }
}
