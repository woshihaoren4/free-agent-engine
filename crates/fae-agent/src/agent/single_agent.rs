use crate::memory::Memory;
use crate::planner::{AgentPlanningExt, Planning};
use crate::{
    AgentConfig, Command, Env, EnvEvent, Event, MemoryItem, MemoryRole, MemoryRuler, Message,
    NonePlan, PlanningResult, SenderMessageStream, SessionConfig, SessionInfo, Task, TaskResult,
    TaskType, define_planning_group,
};
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs, ChatCompletionResponseStream,
    CreateChatCompletionRequest, CreateChatCompletionRequestArgs,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_stream::StreamExt;
use wd_tools::PFErr;

#[derive(Default, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SingleAgentSessionConfig {
    pub id: String,
    pub name: String,
}

pub struct SingleAgent<M> {
    agent_id: String,
    memory: Arc<dyn Memory<M> + Send + 'static>,
    session_config: Arc<dyn SessionConfig<SingleAgentSessionConfig> + Send + 'static>,
    agent_config: Arc<dyn AgentConfig + Send + 'static>,
}

impl<M> SingleAgent<M> {
    pub fn new(
        agent_id: impl Into<String>,
        memory: Arc<dyn Memory<M> + Send + 'static>,
        session_config: Arc<dyn SessionConfig<SingleAgentSessionConfig> + Send + 'static>,
        agent_config: Arc<dyn AgentConfig + Send + 'static>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            memory,
            session_config,
            agent_config,
        }
    }
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
    agent_config: Arc<dyn AgentConfig + Send + 'static>,
    doing: Vec<String>,
}

impl<M> SingleAgentPlanSessionCallStream<M>
where
    M: MemoryRuler + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    pub fn new(
        env: Env,
        agent_config: Arc<dyn AgentConfig + Send + 'static>,
        memory: Arc<dyn Memory<M> + Send + 'static>,
        session_info: SessionInfo,
        message_id: String,
        input: M,
        output: SenderMessageStream<M>,
    ) -> Self {
        let id = wd_tools::uuid::v4();
        Self {
            env,
            id,
            session_info,
            message_id,
            input,
            output,
            memory,
            agent_config,
            doing: Vec::new(),
        }
    }
    pub async fn build_openai_api_request(&self) -> anyhow::Result<CreateChatCompletionRequest> {
        let model = self.agent_config.model().await?;
        let mut req = CreateChatCompletionRequestArgs::default();
        req.model(model.model.as_str());
        req.stream(true);
        if let Some(temperature) = model.temperature {
            req.temperature(temperature);
        }
        if let Some(top_p) = model.top_p {
            req.top_p(top_p);
        }
        if let Some(presence_penalty) = model.presence_penalty {
            req.presence_penalty(presence_penalty);
        }
        if let Some(max_completion_tokens) = model.max_completion_tokens {
            req.max_completion_tokens(max_completion_tokens);
        }
        let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();
        //添加prompt
        let prompt = self.agent_config.prompt().await?;
        messages.push(
            ChatCompletionRequestSystemMessageArgs::default()
                .content(prompt)
                .build()
                .expect("build message failed!")
                .into(),
        );
        //添加历史消息,最大10条
        for item in self
            .memory
            .load(self.session_info.session_id.as_str(), 0, 10)
            .await?
        {
            let content = item.content.as_content();
            let msg: ChatCompletionRequestMessage = match item.role {
                MemoryRole::System => {
                    // ChatCompletionRequestSystemMessageArgs::default().content(content).build()?.into()
                    continue;
                }
                MemoryRole::User => ChatCompletionRequestUserMessageArgs::default()
                    .content(content)
                    .build()?
                    .into(),
                MemoryRole::Assistant => {
                    async_openai::types::ChatCompletionRequestAssistantMessageArgs::default()
                        .content(content)
                        .build()?
                        .into()
                }
                _ => continue,
            };
            messages.push(msg);
        }
        // 添加用户query
        messages.push(
            ChatCompletionRequestUserMessageArgs::default()
                .content(self.input.as_content())
                .build()
                .expect("build message failed!")
                .into(),
        );
        // 添加中间调用的消息
        for item in self.doing.iter() {
            messages.push(
                async_openai::types::ChatCompletionRequestAssistantMessageArgs::default()
                    .content(item.as_str())
                    .build()?
                    .into(),
            );
        }

        req.messages(messages);

        Ok(req.build()?)
    }
}

#[async_trait::async_trait]
impl<M> Planning for SingleAgentPlanSessionCallStream<M>
where
    M: MemoryRuler + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    fn id(&self) -> String {
        self.id.clone()
    }
    async fn init(&mut self) -> anyhow::Result<PlanningResult> {
        let exec_channel = self.agent_config.channel().await?;
        let req = self.build_openai_api_request().await?;
        let task = Task::default()
            .set_type(TaskType::Model)
            .set_agent_id(self.id.clone())
            .set_args(req)
            .set_exec_channel(exec_channel);
        Ok(PlanningResult::Tasks(vec![task]))
    }

    async fn next(&mut self, mut event: TaskResult) -> anyhow::Result<PlanningResult> {
        let mut full_response = String::new();
        if let Some(mut s) = event.into_inner::<ChatCompletionResponseStream>() {
            //持续输出
            while let Some(chunk) = s.next().await {
                let chunk = chunk?;
                if let Some(choice) = chunk.choices.first() {
                    if let Some(content) = &choice.delta.content {
                        full_response.push_str(content);
                        let msg = M::from_content(content.clone());
                        self.output.send(&self.message_id, msg).await?;
                    }
                }
            }
            self.output.close();
        } else {
            return anyhow::anyhow!("[SingleAgent] task result unknown, {:?}", event).err();
        }
        // 输入和输出内容添加到memory
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.memory
            .push(MemoryItem {
                id: self.message_id.clone(),
                session_id: self.session_info.session_id.clone(),
                timestamp,
                role: MemoryRole::User,
                content: self.input.clone(),
            })
            .await?;

        self.memory
            .push(MemoryItem {
                id: wd_tools::uuid::v4(),
                session_id: self.session_info.session_id.clone(),
                timestamp,
                role: MemoryRole::Assistant,
                content: M::from_content(full_response),
            })
            .await?;

        // self.memory.flush().await?;
        return Ok(PlanningResult::End(None));
    }
}

define_planning_group!(
    pub enum SingleAgentPlan<M>
    {
        // SessionCall(SingleAgentPlanSessionCall<M>),
        None(NonePlan),
        SessionCallStream(SingleAgentPlanSessionCallStream<M>),
    }
    where M:MemoryRuler + Serialize + DeserializeOwned + Clone+Send + Sync + 'static
);

#[async_trait::async_trait]
impl<M: MemoryRuler + Serialize + DeserializeOwned + Clone + Send + Sync + 'static>
    AgentPlanningExt<SingleAgentPlan<M>> for SingleAgent<M>
{
    fn id(&self) -> String {
        self.agent_id.clone()
    }

    async fn generate_plan(&self, env: Env, event: Event) -> anyhow::Result<SingleAgentPlan<M>> {
        let (info, mut msg, output) = match event {
            Event::None => return Ok(SingleAgentPlan::None(NonePlan)),
            Event::SessionCall(_, _) => {
                return anyhow::anyhow!("[SingleAgent] SessionCall not supported").err();
            }
            Event::SessionCallStream(info, msg, output) => (info, msg, output),
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
        let input: M = if let Some(s) = msg.try_into_inner() {
            s
        } else {
            return anyhow::anyhow!("[SingleAgent] SessionCallStream input unknown").err();
        };
        // session 没有则创建一个
        if self
            .session_config
            .load(info.session_id.as_str())
            .await?
            .is_none()
        {
            self.session_config
                .create(SingleAgentSessionConfig {
                    id: info.session_id.clone(),
                    name: input.as_content(),
                })
                .await?;
        }
        let memory = self.memory.clone();
        let output = Event::sender_message_to_stream_t(output);
        let plan = SingleAgentPlan::SessionCallStream(SingleAgentPlanSessionCallStream::new(
            env,
            self.agent_config.clone(),
            memory,
            info,
            msg.id,
            input,
            output,
        ));
        Ok(plan)
    }

    async fn exit(&self) {
        if let Err(e) = self.memory.flush().await {
            wd_log::log_error_ln!("[SingleAgent:exit] flush memory error: {:?}", e);
        }
    }
}
