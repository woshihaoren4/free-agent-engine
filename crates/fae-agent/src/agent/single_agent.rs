use crate::define::SenderMessageStream;
use crate::memory::MemoryMessageExt;
use crate::planner::{AgentEventHandle, Planning};
use crate::{AgentConfig, ChatMsg, Context, Env, FAE_HOME, McpToolRequest, McpTools, Memory, MemoryEntry, NonePlan, PlanningResult, SessionCtl, SessionCtlExt, SessionMetadata, GLOBAL_KEY_WORKSPACE, Task, TaskResult, TaskType, ThingItem, ThingSelect, ToolOut, ToolRequest, define_planning_group, fae_home, McpToolResult, ToolResponse, ToolRespItem, GLOBAL_KEY_SESSION_ID, TimedTask};
use async_openai::types::chat::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs, ChatCompletionResponseStream, ChatCompletionTool,
    ChatCompletionTools, CreateChatCompletionRequest, CreateChatCompletionRequestArgs,
    FunctionObjectArgs, ReasoningEffort,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_stream::StreamExt;
use wd_tools::PFErr;

pub struct SingleAgent<S, M> {
    agent_id: String,
    memory: Arc<dyn MemoryMessageExt<M> + Send + 'static>,
    session_config: Arc<dyn SessionCtlExt<S> + Send + 'static>,
    agent_config: Arc<dyn AgentConfig + Send + 'static>,
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
pub struct SingleAgentPlanSessionCallStream<S,M> {
    id: String,
    agent_id: String,
    session_md:S,
    env: Env,
    output: Option<SenderMessageStream<M>>,
    output_title:String,
    output_content:String,
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
}

impl<S,M> SingleAgentPlanSessionCallStream<S,M>
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
        }
    }
    fn set_output(&mut self, output: SenderMessageStream<M>) {
        self.output = Some(output);
    }
    fn additional_session_tips(&mut self,mut tips:String){
        tips.push_str(self.agent_info.as_str());
        self.agent_info = tips;
    }
    fn extend_context(&self,map:HashMap<String,String>){
        for (k,v) in map{
            self.context.set(k,v);
        }
    }
    async fn send(&mut self,msg:M)->anyhow::Result<()>{
        if let Some(ref output) = self.output {
            output.send(msg).await?;
        }else{
            let title = msg.title();
            if title != self.output_title{
                if !self.output_title.is_empty() {
                    let content = self.output_content.replace("\n","\r\n");
                    println!("\r\n---[{}:{}]-> {}:\r\n{}\r\n",self.agent_id,self.session_md.id(), self.output_title,content);
                }
                self.output_title = title;
                self.output_content = String::new();
            }
            self.output_content.push_str(msg.content());
        }
        Ok(())
    }
    pub fn close(&self) {
        if let Some(ref output) = self.output{
            output.close();
        }else{
            if !self.output_content.is_empty() {
                let content = self.output_content.replace("\n","\r\n");
                println!("\r\n---[{}:{}]-> {}:\r\n{}\r\n",self.agent_id,self.session_md.id(), self.output_title,content);
            }
        }
    }
    fn get_timestamp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
    pub async fn build_openai_api_request(&self) -> anyhow::Result<CreateChatCompletionRequest> {
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
        let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();
        //添加prompt
        let mut prompt = self.agent_config.prompt();
        prompt.push_str("\n");
        prompt.push_str(self.agent_info.as_str());
        messages.push(
            ChatCompletionRequestSystemMessageArgs::default()
                .content(prompt)
                .build()
                .expect("build message failed!")
                .into(),
        );
        //添加历史消息
        for item in self
            .memory
            .load_ext(
                &self.session_md.user_id(),
                self.session_md.id(),
                0,
                model.max_chat_history_round as usize,
            )
            .await?
        {
            if let Some(msg) = item.to_openai_message() {
                messages.push(msg);
            }
        }
        // 添加中间调用的消息
        for item in self.exec_records.iter() {
            if let Some(msg) = item.clone().to_openai_message() {
                messages.push(msg);
            }
        }
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

    pub async fn make_model_task(&self) -> anyhow::Result<PlanningResult> {
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
        let agent_metadata = self.agent_config.metadata(&self.agent_id);
        let memory_metdata = self
            .memory
            .metadata_ext(
                &self.session_md.user_id(),
                self.session_md.id(),
            )
            .await?;
        let mut info = agent_metadata + &memory_metdata;
        info.push_str(self.agent_info.as_str());
        self.agent_info = info;
        Ok(())
    }
    pub async fn load_skills(&mut self) -> anyhow::Result<()> {
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
        let info = self
            .memory
            .get_user_info_ext(&self.session_md.user_id())
            .await?;
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
    pub fn parse_mcp_info(&self,tool_name:String)->anyhow::Result<(String,String)>{
        for i in self.mcp_tools.iter(){
            if i.1.name == tool_name{
                return Ok((i.0.clone(), tool_name));
            }
        }
        anyhow::anyhow!("[SingleAgent:{}] unknown tool: {:?}", self.agent_id, tool_name).err()
    }
    pub fn tool_call_to_task(&self,tool_name:String,arguments:String) -> anyhow::Result<Task>{
        let is_tool = self.tools.get(tool_name.as_str()).is_some();
        let (channel, name) = if is_tool {
            Self::parse_tool_channel(tool_name.as_str())
        }else{
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
        if let Some(resp) = event.into_inner::<McpToolResult>(){
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
        }else if let Some(mut tool_resp) = event.into_inner::<ToolResponse>() {
            while let result = tool_resp.next().await? {
                match result {
                    ToolRespItem::Streaming(item) => {
                        tool_out.set_output(item);
                        let mut record = M::from_openai_msg(ChatMsg::Tool(tool_out.clone()));
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
                let tool_task = self.tool_call_to_task(tool.tool_name,tool.arguments)?;
                self.doing.insert(tool_task.get_id().to_string(), tool_out);
                tool_tasks.push(tool_task);
            } else {
                self.exec_records.push(i);
            }
        }
        if !tool_tasks.is_empty() {
            return Ok(PlanningResult::Tasks(tool_tasks));
        }
        // 仅为模型输出
        for msg in std::mem::take(&mut self.exec_records) {
            if msg.is_remember() {
                self.memory
                    .push_ext(
                        &self.session_md.user_id(),
                        self.session_md.id(),
                        msg,
                    )
                    .await?;
            }
        }
        self.close();
        return Ok(PlanningResult::End(None));
    }
}

#[async_trait::async_trait]
impl<S, M> Planning for SingleAgentPlanSessionCallStream<S,M>
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
    S: SessionMetadata + Clone + Send + Sync + 'static,
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
        let mut plan = SingleAgentPlanSessionCallStream::<S,M>::new(
            env,
            agent_id,
            info.clone(),
            self.agent_config.clone(),
            memory,
            input,
        );
        plan.set_output(output);
        if let Some(map) = info.extend(){
            plan.extend_context(map);
        }
        if let Some(tips) = info.additional_tips(){
            plan.additional_session_tips(tips);
        }
        Ok(SingleAgentPlan::SessionCallStream(plan))
    }

    async fn on_timed(&self, env: Env, task: TimedTask) -> anyhow::Result<()> {
        let user_input = format!("When a user's scheduled task expires, you must execute it.\nImportant: Users will not receive your output; you must send it to them via notification.\nTask Details：\n{}",task.task_content);
        let mut msgs = M::from_openai_msg(ChatMsg::with_user(user_input));
        let input = if let Some(msg) = msgs.pop() {
            msg
        } else {
            return anyhow::anyhow!("[SingleAgent:on_timed] Failed to create input message").err();
        };
        // session必须存在，如果删了，则任务没有意义
        let info = if let Some(s) = self.session_config.load_ext(&task.user_id, &task.session_id).await? {
            s
        } else {
            return anyhow::anyhow!("[SingleAgent:on_timed] Session not found").err();
        };
        let memory = self.memory.clone();
        let mut plan = SingleAgentPlanSessionCallStream::<S,M>::new(
            env.clone(),
            task.agent_id.clone(),
            info.clone(),
            self.agent_config.clone(),
            memory,
            input,
        );
        if let Some(map) = info.extend(){
            plan.extend_context(map);
        }
        if let Some(tips) = info.additional_tips(){
            plan.additional_session_tips(tips);
        }
        let plan: Box<dyn Planning + Send + 'static> = Box::new(plan);
        let task = Task::new(plan.get_context(),plan.id(),task.agent_id,TaskType::Plan).set_args(plan);
        env.spawn(vec![task]).await
    }

    async fn exit(&self) {
        if let Err(e) = self.memory.flush().await {
            wd_log::log_error_ln!("[SingleAgent:exit] flush memory error: {:?}", e);
        }
    }
}
