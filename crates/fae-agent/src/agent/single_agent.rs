use crate::define::SenderMessageStream;
use crate::memory::MemoryMessageExt;
use crate::planner::{AgentEventHandle, Planning};
use crate::{
    AgentConfig, AgentTask, AgentTaskStatus, ChatMsg, Context, Env, FAE_HOME,
    GLOBAL_KEY_SESSION_ID, McpToolRequest, McpToolResult, Memory, MemoryEntry, NonePlan,
    PlanningResult, SessionCtl, SessionCtlExt, SessionMetadata, Task, TaskResult, TaskType,
    ThingItem, ThingSelect, TimedTask, ToolOut, ToolRequest, ToolRespItem, ToolResponse, Trigger,
    define_planning_group,
};
use async_openai::types::chat::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs, ChatCompletionResponseStream, ChatCompletionTool,
    ChatCompletionTools, CreateChatCompletionRequest, CreateChatCompletionRequestArgs,
    FunctionObjectArgs, ReasoningEffort,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use tokio_stream::StreamExt;
use wd_tools::PFErr;

const COMPACT_PROMPT: &str = "Please compress the current conversation context, preserving only the information necessary to continue the task later. Remove repetition, small talk, low-value reasoning, and already-resolved intermediate steps.
The compressed result should include:
1. The user’s final goal and explicit requirements
2. Work completed so far and key decisions made
3. Remaining tasks
4. Important file paths, commands, configurations, APIs, and constraints
5. Discovered issues, errors, test results, and risks
6. Preferences or limitations that must be followed later
Requirements:
- Keep facts accurate and do not introduce new assumptions
- Use concise bullet points
- Preserve actionable details and omit irrelevant process
- If any information is unconfirmed, clearly mark it as “unconfirmed”
- Output only the compressed context; do not explain the compression method";

pub struct SingleAgent<S, M> {
    agent_id: String,
    memory: Arc<dyn MemoryMessageExt<M> + Send + 'static>,
    session_config: Arc<dyn SessionCtlExt<S> + Send + 'static>,
    agent_config: Arc<dyn AgentConfig + Send + 'static>,
}
impl<S, M> Debug for SingleAgent<S, M> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "SingleAgent {:?}", self.agent_id)
    }
}

impl<S, M> SingleAgent<S, M> {
    pub fn new(
        agent_id: impl Into<String>,
        memory: Arc<dyn MemoryMessageExt<M> + Send + 'static>,
        session_config: Arc<dyn SessionCtlExt<S> + Send + 'static>,
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

#[derive(Debug)]
pub struct SingleAgentPlanSessionCallStream<S, M> {
    id: String,
    agent_id: String,
    session_md: S,
    env: Env,
    output: Option<SenderMessageStream<M>>,
    output_title: String,
    output_content: String,
    memory: Arc<dyn MemoryMessageExt<M> + Send + Sync + 'static>,
    agent_config: Arc<dyn AgentConfig + Send + 'static>,
    exec_records: Vec<M>,
    // agent 信息，包括空间路径，配置路径等
    agent_info: String,
    //执行工具,task_id,tool
    doing: HashMap<String, ToolOut>,
    //工具描述,channel__tool_name, desc,args
    tools: HashMap<String, (String, Value)>,
    //mcp, 渠道：tool name, tool detail
    mcp_tools: Vec<(String, McpToolRequest)>,
    //执行上下文
    context: Context,
    // 上下文压缩,true:压缩,false:不压缩
    compact_context: bool,
}

impl<S, M> SingleAgentPlanSessionCallStream<S, M>
where
    S: SessionMetadata + Sync + Send + 'static,
    M: MemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    pub fn new(
        env: Env,
        agent_id: String,
        session_md: S,
        agent_config: Arc<dyn AgentConfig + Send + 'static>,
        memory: Arc<dyn MemoryMessageExt<M> + Send + 'static>,
        input: M,
    ) -> Self {
        let id = wd_tools::uuid::v4();
        let context = Context::new(env.clone());
        context.set(GLOBAL_KEY_SESSION_ID, session_md.id().to_string());
        let mut exec_records = Vec::new();
        exec_records.push(input);
        Self {
            context,
            agent_id,
            session_md,
            env,
            id,
            memory,
            agent_config,
            exec_records,
            output: None,
            output_title: String::new(),
            output_content: String::new(),
            doing: HashMap::new(),
            tools: HashMap::new(),
            mcp_tools: Vec::new(),
            agent_info: String::new(),
            compact_context: false,
        }
    }
    fn set_output(&mut self, output: SenderMessageStream<M>) {
        self.output = Some(output);
    }
    fn additional_session_tips(&mut self, mut tips: String) {
        tips.push_str(self.agent_info.as_str());
        self.agent_info = tips;
    }
    fn extend_context(&self, map: HashMap<String, String>) {
        for (k, v) in map {
            self.context.set(k, v);
        }
    }
    async fn send(&mut self, mut msg: M) -> anyhow::Result<()> {
        msg.set_agent_id(self.agent_id.clone());
        if let Some(ref output) = self.output {
            output.send(msg).await?;
        } else {
            let title = msg.title();
            if title != self.output_title {
                if !self.output_title.is_empty() {
                    let content = self.output_content.replace("\n", "\r\n");
                    println!(
                        "\r\n---[{}:{}]-> {}:\r\n{}\r\n",
                        self.agent_id,
                        self.session_md.id(),
                        self.output_title,
                        content
                    );
                }
                self.output_title = title;
                self.output_content = String::new();
            }
            self.output_content.push_str(msg.content());
        }
        Ok(())
    }
    pub fn close(&mut self) {
        if let Some(output) = self.output.take() {
            Trigger::agent_call_session_over(self.agent_id.as_str(), self.session_md.id(), output);
        } else {
            if !self.output_content.is_empty() {
                let content = self.output_content.replace("\n", "\r\n");
                println!(
                    "\r\n---[{}:{}]-> {}:\r\n{}\r\n",
                    self.agent_id,
                    self.session_md.id(),
                    self.output_title,
                    content
                );
            }
        }
    }
    #[allow(dead_code)]
    fn get_timestamp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
    pub async fn start_compact(
        &mut self,
        mut req: CreateChatCompletionRequestArgs,
        mut messages: VecDeque<ChatCompletionRequestMessage>,
    ) -> anyhow::Result<CreateChatCompletionRequest> {
        // 修改上下文压缩状态
        self.compact_context = true;
        // 组装上下文压缩提示词
        messages.push_front(
            ChatCompletionRequestSystemMessageArgs::default()
                .content(COMPACT_PROMPT)
                .build()
                .expect("build message failed!")
                .into(),
        );
        messages.push_back(
            ChatCompletionRequestUserMessageArgs::default()
                .content("The current context is too long; you must perform a compression process.")
                .build()
                .expect("build message failed!")
                .into(),
        );
        req.messages(messages);

        //发送通知
        self.send(MemoryEntry::from_custom_msg(
            "Compacting".to_string(),
            "Context length exceeds limit, start compressing context...".to_string(),
        ))
        .await?;
        Ok(req.build()?)
    }
    pub async fn end_compact(&mut self) -> anyhow::Result<PlanningResult> {
        self.compact_context = false;
        // 获取压缩后的结果
        let mut compact_text = String::new();
        while let Some(msg) = self.exec_records.pop() {
            if let Some(text) = msg.try_to_model() {
                compact_text = text;
                break;
            }
        }
        if compact_text.is_empty() {
            return anyhow::anyhow!("[SingleAgent::{}]compact text is empty", self.agent_id).err();
        }
        //清理执行记录
        self.memory
            .on_reset(self.session_md.user_id(), self.session_md.id())
            .await?;
        self.doing.clear();
        self.exec_records.clear();
        //添加压缩后的结果
        self.exec_records.push(
            M::from_openai_msg(ChatMsg::with_user(compact_text))
                .pop()
                .unwrap(),
        );
        //发送通知
        self.send(MemoryEntry::from_custom_msg(
            "Compacted".to_string(),
            "Context compression completed.".to_string(),
        ))
        .await?;
        // 重新生成任务
        self.make_model_task().await
    }
    pub async fn build_openai_api_request(
        &mut self,
    ) -> anyhow::Result<CreateChatCompletionRequest> {
        let model = self.agent_config.model();
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
        #[allow(deprecated)]
        if let Some(max_tokens) = model.max_tokens {
            req.max_tokens(max_tokens);
        }
        if let Some(l) = model.reasoning_effort {
            match l {
                1 => {
                    req.reasoning_effort(ReasoningEffort::Minimal);
                }
                2 => {
                    req.reasoning_effort(ReasoningEffort::Low);
                }
                3 => {
                    req.reasoning_effort(ReasoningEffort::Medium);
                }
                4 => {
                    req.reasoning_effort(ReasoningEffort::High);
                }
                _ => {}
            }
        }
        // 组装上下文
        let mut messages: VecDeque<ChatCompletionRequestMessage> = VecDeque::new();
        let mut context_size = 0;
        //添加历史消息
        for item in self
            .memory
            .on_load(
                &self.session_md.user_id(),
                self.session_md.id(),
                0,
                model.max_chat_history_round as usize,
            )
            .await?
        {
            context_size += item.size();
            if let Some(msg) = item.to_openai_message() {
                messages.push_back(msg);
            }
        }
        // 添加中间调用的消息
        for item in self.exec_records.iter() {
            context_size += item.size();
            if let Some(msg) = item.clone().to_openai_message() {
                messages.push_back(msg);
            }
        }
        // 组装prompt
        let mut prompt = self.agent_config.prompt();
        prompt.push_str("\n");
        prompt.push_str(self.agent_info.as_str());
        context_size += prompt.chars().count();
        // 压缩判断
        if let Some(min_compact_window_size) = model.min_compact_window_size {
            // wd_log::log_info_ln!("context size: {}, min compact window size: {}", context_size, min_compact_window_size);
            if context_size as u32 >= min_compact_window_size {
                //触发上下文压缩
                return self.start_compact(req, messages).await;
            }
        }
        //添加prompt
        messages.push_front(
            ChatCompletionRequestSystemMessageArgs::default()
                .content(prompt)
                .build()
                .expect("build message failed!")
                .into(),
        );
        req.messages(messages);
        // 添加工具
        let mut tools = Vec::new();
        for (chan__name, (desc, args)) in self.tools.iter() {
            tools.push(ChatCompletionTools::Function(ChatCompletionTool {
                function: FunctionObjectArgs::default()
                    .name(chan__name.clone())
                    .description(desc.clone())
                    .parameters(args.clone())
                    .build()?,
            }))
        }
        // 添加mcp工具
        for (_c, tool) in self.mcp_tools.iter() {
            tools.push(ChatCompletionTools::Function(ChatCompletionTool {
                function: FunctionObjectArgs::default()
                    .name(tool.name.clone())
                    .description(tool.get_description())
                    .parameters(tool.arguments().clone())
                    .build()?,
            }))
        }
        req.tools(tools);

        Ok(req.build()?)
    }

    pub async fn make_model_task(&mut self) -> anyhow::Result<PlanningResult> {
        let exec_channel = self.agent_config.model().channel;
        let req = self.build_openai_api_request().await?;
        let task = Task::new(
            self.context.clone(),
            self.id.clone(),
            self.id.clone(),
            TaskType::Model,
        )
        .set_args(req)
        .set_exec_channel(exec_channel);
        Ok(PlanningResult::Tasks(vec![task]))
    }
    pub async fn init_agent_info(&mut self) -> anyhow::Result<()> {
        let agent_metadata = self
            .agent_config
            .metadata(self.env.clone(), &self.session_md.user_id(), &self.agent_id)
            .await;
        let memory_metdata = self
            .memory
            .on_metadata(&self.session_md.user_id(), self.session_md.id())
            .await?;
        let mut info = agent_metadata + &memory_metdata;
        info.push_str(self.agent_info.as_str());
        self.agent_info = info;
        Ok(())
    }
    pub async fn load_sub_agents(&mut self) -> anyhow::Result<()> {
        if self.agent_config.sub_agents().is_empty() {
            return Ok(());
        }
        let agents = self.agent_config.sub_agents();
        let mut info = "\n---\n## Sub-agents:\nYou can add an agent to the `sub_agent` field in your configuration file. Example field format: [\"agent_a\", \"agent_b\"].\nEach subagent is an expert in a specific field. If you need to handle tasks in the corresponding field, you must initiate a call to them through `agent_exec_task`.".to_string();
        info.push_str("\n<sub_agent_list>\n>");
        for agent in agents {
            let things = self
                .env
                .query(ThingSelect::Agent(agent.clone()).into())
                .await?;
            for mut i in things {
                while let Some(ThingItem::Agent(id, desc)) = i.items.pop() {
                    info.push_str(format!("\n - AgentID: {} ,specialty: {}", id, desc).as_str());
                }
            }
        }
        info.push_str("\n</sub_agent_list>");
        self.agent_info.push_str(&info);
        Ok(())
    }
    pub async fn load_skills(&mut self) -> anyhow::Result<()> {
        if self.agent_config.skills().is_empty() {
            return Ok(());
        }
        let mut info = format!(
            "\n---\n## Skills (mandatory)\nYou can find and install your skill in its directory. $SKILL_DIR=${}/skills",
            FAE_HOME
        );
        info.push_str("\nBefore reply: scan all entries inside <available_skills>");
        info.push_str("\n - If exactly one skill matches: use read_file(path=$SKILL_PATH) to read the full content of SKILL.md and strictly follow its instructions");
        info.push_str("\n - If multiple skills match: select only the most relevant one, then use read to load it");
        info.push_str("\n - If no skills match: do not read any skill files");
        info.push_str("\n<available_skills>");
        let skills = self.agent_config.skills();
        for skill in skills {
            let things = self
                .env
                .query(ThingSelect::Skill(skill.channel.clone(), skill.name.clone(), None).into())
                .await?;
            for mut i in things {
                while let Some(ThingItem::Skill(header)) = i.items.pop() {
                    info.push_str(format!("\n - {}", header.format()).as_str());
                }
            }
        }
        info.push_str("\n</available_skills>");
        self.agent_info.push_str(&info);
        Ok(())
    }
    pub async fn load_mcp(&mut self) -> anyhow::Result<()> {
        if self.agent_config.mcp_servers().is_empty() {
            return Ok(());
        }
        let mcp_servers = self.agent_config.mcp_servers();
        for mcp_server in mcp_servers {
            let things = self
                .env
                .query(ThingSelect::Mcp(mcp_server.channel.clone(), mcp_server.name.clone()).into())
                .await?;
            for thing in things {
                for item in thing.items {
                    if let ThingItem::Mcp(tools) = item {
                        for i in tools {
                            self.mcp_tools.push((mcp_server.channel.clone(), i));
                        }
                    }
                }
            }
        }
        // 添加到prompt
        let mut info = "\n---\n## MCP Metadata".to_string();
        info.push_str("\nMCP configuration path = `$FAE_HOME/mcp/mcp_list.json`.**But you can not use them.**");
        info.push_str("\nThe MCP name you can use in path：$AGENT_CONFIG_PATH.mcp_servers.you can enable mcp name to the file。 format:[{\"name\":\"mcp_name\"}].");
        // info.push_str("\n<mcp_tools>");
        // for t in self.mcp_tools.iter() {
        //     info.push_str(t.1.format().as_str());
        // }
        // info.push_str("\n</mcp_tools>");
        self.agent_info.push_str(&info);
        Ok(())
    }
    pub async fn load_tools(&mut self) -> anyhow::Result<()> {
        let tools = self.agent_config.tools();
        for tool in tools {
            let mut info = self
                .env
                .query(ThingSelect::Tool(tool.channel.clone(), tool.name.clone()).into())
                .await?;
            if let Some(mut ting) = info.pop() {
                if let Some(ThingItem::Tool(desc, args)) = ting.items.pop() {
                    self.tools
                        .insert(format!("{}__{}", tool.channel, tool.name), (desc, args));
                    continue;
                }
            }
            return anyhow::anyhow!(
                "[SingleAgent:{}:{}] no tools found, {:?}",
                self.agent_id,
                self.id,
                tool
            )
            .err();
        }
        Ok(())
    }
    pub async fn load_memory(&mut self) -> anyhow::Result<()> {
        // 添加user_info
        let info = self.memory.on_user_info(&self.session_md.user_id()).await?;
        self.agent_info.push_str(&info);
        Ok(())
    }
    // (渠道，函数名)
    pub fn parse_tool_channel(tool_name: &str) -> (String, String) {
        let ss = tool_name.splitn(2, "__").collect::<Vec<&str>>();
        if ss.len() >= 2 {
            (ss[0].to_string(), ss[1].to_string())
        } else {
            ("default".to_string(), tool_name.to_string())
        }
    }
    pub fn parse_mcp_info(&self, tool_name: String) -> anyhow::Result<(String, String)> {
        for i in self.mcp_tools.iter() {
            if i.1.name == tool_name {
                return Ok((i.0.clone(), tool_name));
            }
        }
        anyhow::anyhow!(
            "[SingleAgent:{}] unknown tool: {:?}",
            self.agent_id,
            tool_name
        )
        .err()
    }
    pub fn tool_call_to_task(&self, tool_name: String, arguments: String) -> anyhow::Result<Task> {
        let is_tool = self.tools.get(tool_name.as_str()).is_some();
        let (channel, name) = if is_tool {
            Self::parse_tool_channel(tool_name.as_str())
        } else {
            self.parse_mcp_info(tool_name)?
        };
        let tool_req = ToolRequest::new(name, arguments);
        let mut task = Task::with_content(self.context.clone())
            .set_agent_id(self.agent_id.clone())
            .set_user_id(self.session_md.user_id().to_string())
            .set_type(TaskType::Tool)
            .set_channel(channel)
            .set_args(tool_req);
        if !is_tool {
            task = task.set_type(TaskType::Mcp);
        }
        Ok(task)
    }

    // 处理工具执行结果
    pub async fn handle_tool_result(
        &mut self,
        mut tool_out: ToolOut,
        mut event: TaskResult,
    ) -> anyhow::Result<PlanningResult> {
        if let Some(resp) = event.into_inner::<McpToolResult>() {
            match resp {
                McpToolResult::Resp(resp) => {
                    tool_out.set_output(resp);
                    let mut record = M::from_openai_msg(ChatMsg::Tool(tool_out));
                    for i in record.clone() {
                        self.send(i).await?;
                    }
                    self.exec_records.append(&mut record);
                }
                McpToolResult::Stream(stream) => {
                    let mut output = String::new();
                    while let Ok(resp) = stream.channel.recv().await {
                        output.push_str(&resp.to_string());
                        output.push_str("\n");
                        let mut tool_out = tool_out.clone();
                        tool_out.set_output(resp.to_string());
                        let record = M::from_openai_msg(ChatMsg::Tool(tool_out));
                        for i in record {
                            self.send(i).await?;
                        }
                    }
                    tool_out.set_output(output);
                    let mut record = M::from_openai_msg(ChatMsg::Tool(tool_out));
                    self.exec_records.append(&mut record);
                }
            }
        } else if let Some(mut tool_resp) = event.into_inner::<ToolResponse>() {
            while let result = tool_resp.next().await? {
                match result {
                    ToolRespItem::Streaming(item) => {
                        tool_out.set_output(item);
                        let record = M::from_openai_msg(ChatMsg::Tool(tool_out.clone()));
                        for i in record.clone() {
                            self.send(i).await?;
                        }
                    }
                    ToolRespItem::Completed(item) => {
                        tool_out.set_output(item);
                        let mut record = M::from_openai_msg(ChatMsg::Tool(tool_out));
                        if !tool_resp.is_streaming() {
                            for i in record.clone() {
                                self.send(i).await?;
                            }
                        }
                        self.exec_records.append(&mut record);
                        break;
                    }
                }
            }
        } else if event.is_success() {
            let tool_resp = format!(
                "The tool[{}] executed successfully. msg={}",
                tool_out.tool_name, event.msg
            );
            tool_out.set_output(tool_resp);
            let mut record = M::from_openai_msg(ChatMsg::Tool(tool_out));
            for i in record.clone() {
                self.send(i).await?;
            }
            self.exec_records.append(&mut record);
        } else {
            return anyhow::anyhow!("[SingleAgent] tool call failed, {:?}", event).err();
        }
        if self.doing.is_empty() {
            // 所有工具调用完成，发起模型调用
            return self.make_model_task().await;
        } else {
            // 还有工具调用，等待
            return Ok(PlanningResult::Tasks(vec![]));
        }
    }
    // 处理模型执行结果
    pub async fn handle_model_result(
        &mut self,
        mut event: TaskResult,
    ) -> anyhow::Result<PlanningResult> {
        let mut records = Vec::new();
        if let Some(mut s) = event.into_inner::<ChatCompletionResponseStream>() {
            //持续输出
            while let Some(chunk) = s.next().await {
                if let Some(msg) = M::stream_append(&mut records, chunk?) {
                    self.send(msg).await?;
                }
            }
        } else {
            return anyhow::anyhow!("[SingleAgent] task result unknown, {:?}", event).err();
        }
        // 调用工具
        let mut tool_tasks = Vec::new();
        for i in records {
            if let Some(tool) = i.try_to_tool_call() {
                // 执行记录
                self.exec_records.push(i.clone());
                // 通知session
                self.send(i).await?;
                // 组装任务
                let tool_out = ToolOut::new(tool.tool_call_id, tool.tool_name.clone());
                let tool_task = self.tool_call_to_task(tool.tool_name, tool.arguments)?;
                self.doing.insert(tool_task.get_id().to_string(), tool_out);
                tool_tasks.push(tool_task);
            } else {
                self.exec_records.push(i);
            }
        }
        if !tool_tasks.is_empty() {
            return Ok(PlanningResult::Tasks(tool_tasks));
        }
        //是否是上下文压缩
        if self.compact_context {
            return self.end_compact().await;
        }
        // 仅为模型输出
        for msg in std::mem::take(&mut self.exec_records) {
            if msg.is_remember() {
                self.memory
                    .on_push(&self.session_md.user_id(), self.session_md.id(), msg)
                    .await?;
            }
        }
        self.close();
        return Ok(PlanningResult::End(None));
    }
}

#[async_trait::async_trait]
impl<S, M> Planning for SingleAgentPlanSessionCallStream<S, M>
where
    S: SessionMetadata + Clone + Send + Sync + 'static,
    M: MemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    fn id(&self) -> String {
        self.id.clone()
    }
    async fn init(&mut self) -> anyhow::Result<PlanningResult> {
        // 初始化agent信息
        self.init_agent_info().await?;
        // 加载子agent
        self.load_sub_agents().await?;
        //加载工具
        self.load_tools().await?;
        //加载skill
        self.load_skills().await?;
        //加载mcp
        self.load_mcp().await?;
        //加载memory
        self.load_memory().await?;
        //发起任务
        // println!("----->\n{}",self.agent_info);
        // return anyhow::anyhow!("<----").err();
        self.make_model_task().await
    }

    async fn next(&mut self, event: TaskResult) -> anyhow::Result<PlanningResult> {
        // 判断是否是工具调用结果
        if let Some(tool_out) = self.doing.remove(&event.task_id) {
            return self.handle_tool_result(tool_out, event).await;
        }
        //调用模型
        return self.handle_model_result(event).await;
    }
    async fn abort(&mut self) {
        wd_log::log_error_ln!(
            "[SingleAgentPlanAbort]::{} aborted, debug info:{}",
            self.agent_id.as_str(),
            self.debug().await
        );
        self.close();
    }
    fn get_context(&self) -> Context {
        self.context.clone()
    }
}

define_planning_group!(
    #[derive(Debug)]
    pub enum SingleAgentPlan<S, M>
    {
        // SessionCall(SingleAgentPlanSessionCall<M>),
        None(NonePlan),
        SessionCallStream(SingleAgentPlanSessionCallStream<S,M>),
    }
    where
    S: SessionMetadata + Clone + Send + Sync + 'static,
    M:MemoryEntry + Serialize + DeserializeOwned + Clone+Send + Sync + 'static
);

#[async_trait::async_trait]
impl<S, M> AgentEventHandle<S, M, M, SingleAgentPlan<S, M>> for SingleAgent<S, M>
where
    S: SessionMetadata + Default + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    M: MemoryEntry + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    fn id(&self) -> String {
        self.agent_id.clone()
    }

    fn desc(&self) -> String {
        self.agent_config.desc()
    }

    async fn on_info(&self) -> Arc<dyn AgentConfig + Send + 'static> {
        self.agent_config.clone()
    }

    async fn on_memory(&self) -> Arc<dyn Memory + Send + 'static> {
        Arc::new(self.memory.clone())
    }

    async fn on_session_ctl(&self) -> Arc<dyn SessionCtl + Send + 'static> {
        Arc::new(self.session_config.clone())
    }

    async fn on_agent_task(&self, env: Env, mut task: AgentTask) -> anyhow::Result<()> {
        //先尝试解析渠道
        let output = task.try_ext_into::<SenderMessageStream<M>>();
        let mut user_id = "".to_string();
        let mut session_id = "".to_string();
        //解析返回值
        let user_input = match task.get_status() {
            AgentTaskStatus::CREATE => {
                user_id = task.executor.user_id;
                session_id = task.executor.session_id;
                format!(
                    "You receive a task from another agent, which you must complete and update the task status upon completion.\n-Task ID：{}\n-Task Details：\n{}",
                    task.task_id, task.content
                )
            }
            AgentTaskStatus::EXECUTING => {
                wd_log::log_info_ln!("[SingleAgent:on_agent_task] Executing task: {:?}", task);
                return Ok(());
            }
            AgentTaskStatus::COMPLETED | AgentTaskStatus::FAILED => {
                user_id = task.author.user_id;
                session_id = task.author.session_id;
                format!(
                    "The task you posted has been completed. \nTask Details: {}\n---\nExecutor: {}\n Task Status: {}\n Result:\n{}",
                    task.content, task.executor.agent_id, task.status, task.result
                )
            }
            _ => {
                return anyhow::anyhow!(
                    "[SingleAgent:on_agent_task] Received an unsupported agent task status {:?}",
                    task
                )
                .err();
            }
        };
        let mut msgs = M::from_openai_msg(ChatMsg::with_user(user_input));
        let input = if let Some(msg) = msgs.pop() {
            msg
        } else {
            return anyhow::anyhow!("[SingleAgent:on_agent_task] Failed to create input message")
                .err();
        };
        //加载session
        if session_id.is_empty() {
            session_id = wd_tools::uuid::v4();
        }
        let info = self
            .session_config
            .must_load_ext(
                user_id.as_str(),
                session_id.as_str(),
                input
                    .content()
                    .chars()
                    .take(10)
                    .collect::<String>()
                    .as_str(),
            )
            .await;
        let memory = self.memory.clone();
        let mut plan = SingleAgentPlanSessionCallStream::<S, M>::new(
            env.clone(),
            self.agent_id.clone(),
            info.clone(),
            self.agent_config.clone(),
            memory,
            input,
        );
        if let Some(out) = output {
            plan.set_output(out);
        }
        if let Some(map) = info.extend() {
            plan.extend_context(map);
        }
        if let Some(tips) = info.additional_tips() {
            plan.additional_session_tips(tips);
        }
        let plan: Box<dyn Planning + Send + 'static> = Box::new(plan);
        let task = Task::new(
            plan.get_context(),
            plan.id(),
            self.agent_id.clone(),
            TaskType::Plan,
        )
        .set_args(plan);
        env.spawn(vec![task]).await
    }

    async fn on_session_call_stream(
        &self,
        env: Env,
        info: &mut S,
        input: M,
        output: SenderMessageStream<M>,
    ) -> anyhow::Result<SingleAgentPlan<S, M>> {
        // session 没有则创建一个
        if self
            .session_config
            .load_ext(&info.user_id(), info.id())
            .await?
            .is_none()
        {
            self.session_config.create_ext(info.clone()).await?;
        }
        let agent_id = self.agent_id.clone();

        let memory = self.memory.clone();
        let mut plan = SingleAgentPlanSessionCallStream::<S, M>::new(
            env,
            agent_id,
            info.clone(),
            self.agent_config.clone(),
            memory,
            input,
        );
        plan.set_output(output);
        if let Some(map) = info.extend() {
            plan.extend_context(map);
        }
        if let Some(tips) = info.additional_tips() {
            plan.additional_session_tips(tips);
        }
        Ok(SingleAgentPlan::SessionCallStream(plan))
    }

    async fn on_task_result_callback(
        &self,
        env: Env,
        mut result: TaskResult,
    ) -> anyhow::Result<()> {
        if let Some(s) = result.into_inner::<AgentTask>() {
            return self.on_agent_task(env, s).await;
        } else {
            wd_log::log_info_ln!(
                "[SingleAgent:on_task_result_callback] Received an unsupported task result status {:?}",
                result
            );
        }
        Ok(())
    }

    async fn on_timed(&self, env: Env, task: TimedTask) -> anyhow::Result<()> {
        let user_input = format!(
            "When a user's scheduled task expires, you must execute it.\nImportant: Users will not receive your output; you must send it to them via notification.\nTask Details：\n{}",
            task.task_content
        );
        let mut msgs = M::from_openai_msg(ChatMsg::with_user(user_input));
        let input = if let Some(msg) = msgs.pop() {
            msg
        } else {
            return anyhow::anyhow!("[SingleAgent:on_timed] Failed to create input message").err();
        };
        // session必须存在，如果删了，则任务没有意义
        let info = if let Some(s) = self
            .session_config
            .load_ext(&task.user_id, &task.session_id)
            .await?
        {
            s
        } else {
            return anyhow::anyhow!("[SingleAgent:on_timed] Session not found").err();
        };
        let memory = self.memory.clone();
        let mut plan = SingleAgentPlanSessionCallStream::<S, M>::new(
            env.clone(),
            task.agent_id.clone(),
            info.clone(),
            self.agent_config.clone(),
            memory,
            input,
        );
        if let Some(map) = info.extend() {
            plan.extend_context(map);
        }
        if let Some(tips) = info.additional_tips() {
            plan.additional_session_tips(tips);
        }
        let plan: Box<dyn Planning + Send + 'static> = Box::new(plan);
        let task =
            Task::new(plan.get_context(), plan.id(), task.agent_id, TaskType::Plan).set_args(plan);
        env.spawn(vec![task]).await
    }

    async fn exit(&self) {
        if let Err(e) = self.memory.flush().await {
            wd_log::log_error_ln!("[SingleAgent:exit] flush memory error: {:?}", e);
        }
    }
}
