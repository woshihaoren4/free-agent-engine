use std::collections::HashMap;
use crate::define::{Msg, SenderMessageStream};
use crate::memory::Memory;
use crate::planner::{AgentEventHandle, Planning};
use crate::{AgentConfig, Env, NonePlan, ChatMsg, MemoryEntry, ModelResponse, PlanningResult, SessionConfig, SessionMetadata, Task, TaskResult, TaskType, define_planning_group, RecordItemToolOut, ThingSelect, ThingItem};
use crate::{EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL, SessionMD};
use async_openai::types::chat::{ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs, ChatCompletionResponseStream, ChatCompletionTool, ChatCompletionTools, CreateChatCompletionRequest, CreateChatCompletionRequestArgs, FunctionObjectArgs};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_stream::StreamExt;
use wd_tools::PFErr;
use async_openai::types::responses::Tool;

#[derive(Default, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SingleAgentSessionConfig {
    pub id: String,
    pub name: String,
}
impl SingleAgentSessionConfig {
    pub fn set_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }
}

impl From<SingleAgentSessionConfig> for SessionMetadata {
    fn from(value: SingleAgentSessionConfig) -> Self {
        SessionMetadata::with_session_id(value.id.as_str()).set_data(value)
    }
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
    records: Vec<M>,
    //执行工具,task_id,tool
    doing: HashMap<String,RecordItemToolOut>,
    //工具描述,channel__tool_name, desc,args
    tools: HashMap<String,(String,String)>,
}

impl<M> SingleAgentPlanSessionCallStream<M>
where
    M: MemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
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
            records: Vec::new(),
            doing: HashMap::new(),
            tools: HashMap::new(),
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
        for item in self.memory.load(self.session_id.as_str(), 0, 10).await? {
            if let Some(msg) = item.to_openai_message() {
                messages.push(msg);
            }
        }
        // 添加用户query
        if let Some(msg) = self.input.clone().to_openai_message() {
            messages.push(msg);
        }
        // 添加中间调用的消息
        for item in self.records.iter() {
            if let Some(msg) = item.clone().to_openai_message() {
                messages.push(msg);
            }
        }
        req.messages(messages);
        // 添加工具调用
        let mut tools = Vec::new();
        for (chan__name,(desc,args)) in self.tools.iter() {
            tools.push(ChatCompletionTools::Function(ChatCompletionTool{
                function: FunctionObjectArgs::default()
                    .name(chan__name.clone())
                    .description(desc.clone())
                    .parameters(args.clone())
                    .build()?
            }))
        }
        req.tools(tools);

        Ok(req.build()?)
    }

    pub async fn make_model_task(&self) -> anyhow::Result<PlanningResult> {
        let exec_channel = self
            .agent_config
            .model()
            .await?
            .channel;
        let req = self.build_openai_api_request().await?;
        let task = Task::default()
            .set_type(TaskType::Model)
            .set_agent_id(self.id.clone())
            .set_args(req)
            .set_exec_channel(exec_channel);
        Ok(PlanningResult::Tasks(vec![task]))
    }
    pub async fn load_tools(&mut self) -> anyhow::Result<()> {
        let tools = self.agent_config.tools().await?;
        for tool in tools {
            let mut info = self.env.query(ThingSelect::Tool(tool.channel.clone(), tool.name.clone())).await?;
            if let Some(mut ting) = info.pop() {
                if let Some(ThingItem::Tool(desc,args)) = ting.items.pop(){
                    self.tools.insert(format!("{}__{}",tool.channel,tool.name), (desc,args));
                    continue
                }
            }
            return anyhow::anyhow!("[SingleAgent::{}] no tools found, {:?}", self.id, tool).err();
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl<M> Planning for SingleAgentPlanSessionCallStream<M>
where
    M: MemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    fn id(&self) -> String {
        self.id.clone()
    }
    async fn init(&mut self) -> anyhow::Result<PlanningResult> {
        //加载工具
        self.load_tools().await?;
        //发起任务
        self.make_model_task().await
    }

    async fn next(&mut self, mut event: TaskResult) -> anyhow::Result<PlanningResult> {

        // 判断是否是工具调用结果
        if let Some(mut tool_out) = self.doing.remove(&event.task_id) {
            if let Some(tool_resp) = event.into_inner::<String>() {
                tool_out.set_output(tool_resp);
                let mut record = M::from_openai_msg(ChatMsg::Tool(tool_out));
                for i in record.clone() {
                    self.output.send(i).await?;
                }
                self.records.append(&mut record);
            }else{
                return anyhow::anyhow!("[SingleAgent] tool call failed, {:?}", event).err();
            }
            if self.doing.is_empty() {
                // 所有工具调用完成，发起模型调用
                return self.make_model_task().await;
            }else{
                // 还有工具调用，等待
                return Ok(PlanningResult::Tasks(vec![]));
            }
        }
        //调用模型
        let mut records = Vec::new();
        if let Some(mut s) = event.into_inner::<ChatCompletionResponseStream>() {
            //持续输出
            while let Some(chunk) = s.next().await {
                if let Some(msg) = M::stream_append(&mut records, chunk?) {
                    self.output.send(msg).await?;
                }
            }
            self.output.close();
        } else {
            return anyhow::anyhow!("[SingleAgent] task result unknown, {:?}", event).err();
        }
        //判断是否有模型调用
        todo!();

        // 输入和输出内容添加到memory
        self.memory
            .push(&self.session_id, self.input.clone())
            .await?;
        for msg in std::mem::take(&mut self.records) {
            self.memory.push(&self.session_id, msg).await?;
        }
        // 合并记录
        for msg in records {
            if msg.is_remember() {
                self.memory.push(&self.session_id, msg).await?;
            }
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
    where M:MemoryEntry + Serialize + DeserializeOwned + Clone+Send + Sync + 'static
);

#[async_trait::async_trait]
impl<M: MemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static>
    AgentEventHandle<SingleAgentSessionConfig, M, M, SingleAgentPlan<M>> for SingleAgent<M>
{
    fn id(&self) -> String {
        self.agent_id.clone()
    }

    async fn on_session_call_stream(
        &self,
        env: Env,
        info: &mut SessionMD<SingleAgentSessionConfig>,
        input: M,
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
                    name: input.content().to_string(),
                })
                .await?;
        }

        let memory = self.memory.clone();
        let plan = SingleAgentPlan::SessionCallStream(SingleAgentPlanSessionCallStream::new(
            env,
            self.agent_config.clone(),
            memory,
            info.session_id.clone(),
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
