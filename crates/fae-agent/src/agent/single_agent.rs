use crate::memory::Memory;
use crate::planner::{AgentPlanningExt, Planning, PlanningItem};
use crate::session::Session;
use crate::{Command, Env, EnvEvent, Event, Message, PlanningResult, SessionInfo, SessionMetaManager, Task, TaskResult, define_planning_group, NonePlan, SenderMessageStream};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use serde::de::DeserializeOwned;
use wd_tools::channel::Sender;
use wd_tools::PFErr;

#[derive(Default, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SingleAgentSessionConfig {
    /// 基础路径，用于存储代理的配置文件和状态文件
    pub base_path: String,
    /// 模型名称,如："gpt-3.5-turbo"
    pub model_name: String,
    /// 模型渠道,如："OpenAI_API"
    pub model_channel: String,
}

pub struct SingleAgent<M> {
    agent_id: String,
    memory: Arc<dyn Memory<M> + Send + 'static>,
    session_manager: Arc<dyn SessionMetaManager<SingleAgentSessionConfig> + Send + 'static>,
}

// pub struct SingleAgentPlanSessionCall<M> {
//     id: String,
//     session_info: SessionInfo,
//     input: M,
//     memory: Arc<dyn Memory<M> + Send + Sync + 'static>,
// }
//
// #[async_trait::async_trait]
// impl<M: Send + Sync + 'static> Planning for SingleAgentPlanSessionCall<M> {
//     fn id(&self) -> String {
//         todo!()
//     }
//
//     async fn init(&mut self) -> anyhow::Result<PlanningResult> {
//         Ok(PlanningResult::End(None))
//     }
//
//     async fn next(&mut self, event: TaskResult) -> anyhow::Result<PlanningResult> {
//         todo!()
//     }
// }

pub struct SingleAgentPlanSessionCallStream<M> {
    id: String,
    env: Env,
    session_info: SessionInfo,
    message_id: String,
    input: M,
    output: SenderMessageStream<M>,
    memory: Arc<dyn Memory<M> + Send + Sync + 'static>,
}

impl<M> SingleAgentPlanSessionCallStream<M> {
    pub fn new(env: Env, memory: Arc<dyn Memory<M> + Send + 'static>, session_info: SessionInfo, message_id: String, input: M, output: SenderMessageStream<M>) -> Self {
        let id = wd_tools::uuid::v4();
        Self {
            env,
            id,
            session_info,
            message_id,
            input,
            output,
            memory,
        }
    }
}

#[async_trait::async_trait]
impl<M: Send + Sync + 'static> Planning for SingleAgentPlanSessionCallStream<M> {
    fn id(&self) -> String {
        self.id.clone()
    }
    async fn init(&mut self) -> anyhow::Result<PlanningResult> {
        Ok(PlanningResult::End(None))
    }

    async fn next(&mut self, event: TaskResult) -> anyhow::Result<PlanningResult> {
        todo!()
    }
}

define_planning_group!(
    pub enum SingleAgentPlan<M> {
        // SessionCall(SingleAgentPlanSessionCall<M>),
        None(NonePlan),
        SessionCallStream(SingleAgentPlanSessionCallStream<M>),
    }
);

#[async_trait::async_trait]
impl<M:  Serialize + DeserializeOwned + Clone +Send + Sync + 'static> AgentPlanningExt<SingleAgentPlan<M>> for SingleAgent<M> {
    fn id(&self) -> String {
        self.agent_id.clone()
    }

    async fn generate_plan(&self, env: Env, event: Event) -> anyhow::Result<SingleAgentPlan<M>> {
        let (info, mut msg, output) = match event {
            Event::None => return Ok(SingleAgentPlan::None(NonePlan)),
            Event::SessionCall(_, _) => {
                return anyhow::anyhow!("[SingleAgent] SessionCall not supported").err();
            }
            Event::SessionCallStream(info, msg, output) => {
                (info, msg, output)
            }
            Event::SessionStreamCall(_, _) => {
                return anyhow::anyhow!("[SingleAgent] SessionStreamCall not supported").err();
            }
            Event::SessionStream(_, _, _) => {
                return anyhow::anyhow!("[SingleAgent] SessionStream not supported").err();
            }
            Event::EnvEvent(_) => {
                return anyhow::anyhow!("[SingleAgent] EnvEvent not supported").err();
            }
            Event::TaskOver(_) => {
                return anyhow::anyhow!("[SingleAgent] TaskOver not supported").err();
            }
            Event::Command(cmd) => {
                if cmd == Command::SystemExit {
                    self.exit().await;
                }
                return Ok(SingleAgentPlan::None(NonePlan));
            }
        };
        let input = if let Some(s) = msg.try_into_inner() {
            s
        } else {
            return anyhow::anyhow!("[SingleAgent] SessionCallStream input unknown").err();
        };
        let memory = self.memory.clone();
        let output = Event::sender_message_to_stream_t(output);
        let plan = SingleAgentPlan::SessionCallStream(SingleAgentPlanSessionCallStream::new(env, memory, info, msg.id, input, output));
        Ok(plan)
    }

    async fn exit(&self) {
        if let Err(e) = self.memory.flush().await {
            wd_log::log_error_ln!("[SingleAgent:exit] flush memory error: {:?}", e);
        }
    }
}
