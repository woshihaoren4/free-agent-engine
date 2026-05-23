use crate::memory::Memory;
use crate::planner::{AgentEventHandle, Planning};
use crate::{AgentConfig, Env, NonePlan, PlanningResult, SessionConfig, Task, TaskResult, TaskType, define_planning_group, OpenAIMemoryEntry, OpenAIChatMsg, OpenAIResponse};
use async_openai::types::chat::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs, ChatCompletionResponseStream,
    CreateChatCompletionRequest, CreateChatCompletionRequestArgs,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_stream::StreamExt;
use wd_tools::PFErr;
use crate::define::{Msg, SenderMessageStream};
use crate::SessionMD;

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
    session_id: String,
    input: M,
    output: SenderMessageStream<M>,
    memory: Arc<dyn Memory<M> + Send + Sync + 'static>,
    agent_config: Arc<dyn AgentConfig + Send + 'static>,
    doing: Vec<M>,
}

impl<M> SingleAgentPlanSessionCallStream<M>
where
    M: OpenAIMemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    pub fn new(
        env: Env,
        agent_config: Arc<dyn AgentConfig + Send + 'static>,
        memory: Arc<dyn Memory<M> + Send + 'static>,
        session_id: String,
        input: M,
        output: SenderMessageStream<M>,
    ) -> Self {
        let id = wd_tools::uuid::v4();
        Self {
            env,
            id,
            session_id,
            input,
            output,
            memory,
            agent_config,
            doing: Vec::new(),
        }
    }
    fn get_timestamp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
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
            .load(self.session_id.as_str(), 0, 10)
            .await?
        {
            if let Some(msg) = item.to_openai_message() {
                messages.push(msg);
            }
        }
        // 添加用户query
        if let Some(msg) = self.input.clone().to_openai_message() {
            messages.push(msg);
        }
        // 添加中间调用的消息
        for item in self.doing.iter() {
            if let Some(msg) = item.clone().to_openai_message() {
                messages.push(msg);
            }
        }
        req.messages(messages);

        Ok(req.build()?)
    }
}

#[async_trait::async_trait]
impl<M> Planning for SingleAgentPlanSessionCallStream<M>
where
    M: OpenAIMemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
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
        let mut records = Vec::new();
        if let Some(mut s) = event.into_inner::<ChatCompletionResponseStream>() {
            //持续输出
            while let Some(chunk) = s.next().await {
                let new_msg = M::stream_append(&mut records,chunk?);
                self.output.send(Msg::new(new_msg)).await?;
            }
            self.output.close();
        } else {
            return anyhow::anyhow!("[SingleAgent] task result unknown, {:?}", event).err();
        }
        // 合并记录
        for msg in records{
            self.memory.push(msg).await?;
        }
        // 输入和输出内容添加到memory
        self.memory
            .push(self.input.clone())
            .await?;
        for msg in std::mem::take(&mut self.doing) {
            self.memory.push(msg).await?;
        }
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
    where M:OpenAIMemoryEntry + Serialize + DeserializeOwned + Clone+Send + Sync + 'static
);

#[async_trait::async_trait]
impl<M: OpenAIMemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static>
    AgentEventHandle<SingleAgentSessionConfig, M, M, SingleAgentPlan<M>> for SingleAgent<M>
{
    fn id(&self) -> String {
        self.agent_id.clone()
    }

    async fn on_session_call_stream(
        &self,
        env: Env,
        info: &mut SessionMD<SingleAgentSessionConfig>,
        input: Msg<M>,
        output: SenderMessageStream<M>,
    ) -> anyhow::Result<SingleAgentPlan<M>> {
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
                    name: input.get_content().content().to_string(),
                })
                .await?;
        }

        let memory = self.memory.clone();
        let plan = SingleAgentPlan::SessionCallStream(SingleAgentPlanSessionCallStream::new(
            env,
            self.agent_config.clone(),
            memory,
            info.session_id.clone(),
            input.content,
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
