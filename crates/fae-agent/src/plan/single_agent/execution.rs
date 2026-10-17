use super::*;

#[derive(Debug, Clone)]
pub(super) struct SingleAgentBinding {
    pub(super) rt: RT,
    pub(super) ctx: Ctx,
    pub(super) template: SingleAgentTemplate,
    pub(super) mode: Option<SingleAgentMode>,
}

#[derive(Debug, Clone)]
pub(in crate::plan) struct SingleAgentTemplate {
    pub(in crate::plan) agent: SingleAgentInfo,
    pub(in crate::plan) prompt: String,
    pub(in crate::plan) model: SingleAgentModelConfig,
    pub(in crate::plan) tool_definitions: Vec<ChatCompletionTools>,
    pub(in crate::plan) tool_routes: HashMap<String, CallableRoute>,
    pub(in crate::plan) ancestor_agents: Vec<String>,
    pub(in crate::plan) hook: Arc<dyn SingleAgentHook>,
}

#[derive(Debug, Clone)]
enum PendingCallKind {
    Tool,
    Mcp,
}

#[derive(Debug)]
struct PendingCall {
    call_id: String,
    tool_name: String,
    kind: PendingCallKind,
    enters_todo_mode: bool,
}

#[derive(Debug, Deserialize)]
struct SubAgentCall {
    agent_id: String,
    input: String,
}

#[derive(Debug, Deserialize)]
struct WorkflowCall {
    workflow_id: String,
}

#[derive(Debug)]
enum SingleAgentStage {
    Memory,
    History,
    Compression,
    Model,
    Tools { remaining: usize },
    Save,
}

type ToolCompletionFormatter = fn(&str) -> anyhow::Result<String>;

#[derive(Debug)]
struct TerminalToolCompletion {
    tool_name: String,
    output_formatter: ToolCompletionFormatter,
}

#[derive(Debug)]
pub(in crate::plan) struct SingleAgentPlan {
    id: String,
    ctx: Ctx,
    template: SingleAgentTemplate,
    input: String,
    images: Vec<String>,
    turn_id: u64,
    session: CommonSession,
    stage: SingleAgentStage,
    user_memory: Vec<UserMemory>,
    messages: Vec<ChatCompletionRequestMessage>,
    additional_history: Vec<SessionMessage>,
    unsaved_messages: Vec<SessionMessage>,
    final_output: String,
    tool_iterations: usize,
    empty_model_retries: usize,
    task_sequence: u64,
    pending_tools: HashMap<String, PendingCall>,
    persist_session: bool,
    terminal_tool_completion: Option<TerminalToolCompletion>,
    pending_terminal_output: Option<String>,
    pending_todo_output: Option<String>,
    todo_plan: Option<Box<SingleAgentTodoPlan>>,
    owns_active_turn: bool,
    finish_on_drop: bool,
}

pub(super) fn build_single_agent_plan(
    ctx: Ctx,
    template: SingleAgentTemplate,
    input: String,
    images: Vec<String>,
    turn_id: u64,
    session: CommonSession,
    mode: Option<SingleAgentMode>,
) -> Box<dyn Plan> {
    match mode {
        Some(SingleAgentMode::Todo) if images.is_empty() => Box::new(SingleAgentTodoPlan::new(
            ctx, template, input, turn_id, session,
        )),
        Some(SingleAgentMode::Todo) => Box::new(SingleAgentTodoPlan::new_with_images(
            ctx, template, input, images, turn_id, session,
        )),
        None if images.is_empty() => {
            Box::new(SingleAgentPlan::new(ctx, template, input, turn_id, session))
        }
        None => Box::new(SingleAgentPlan::new_with_images(
            ctx, template, input, images, turn_id, session,
        )),
    }
}

impl SingleAgentPlan {
    pub(super) fn new(
        ctx: Ctx,
        template: SingleAgentTemplate,
        input: String,
        turn_id: u64,
        session: CommonSession,
    ) -> Self {
        Self::new_with_persistence(
            ctx,
            template,
            input,
            Vec::new(),
            turn_id,
            session,
            true,
            Vec::new(),
            None,
        )
    }

    pub(super) fn new_with_images(
        ctx: Ctx,
        template: SingleAgentTemplate,
        input: String,
        images: Vec<String>,
        turn_id: u64,
        session: CommonSession,
    ) -> Self {
        Self::new_with_persistence(
            ctx,
            template,
            input,
            images,
            turn_id,
            session,
            true,
            Vec::new(),
            None,
        )
    }

    pub(in crate::plan) fn new_ephemeral(
        ctx: Ctx,
        template: SingleAgentTemplate,
        input: String,
        turn_id: u64,
        session: CommonSession,
        additional_history: Vec<SessionMessage>,
    ) -> Self {
        Self::new_with_persistence(
            ctx,
            template,
            input,
            Vec::new(),
            turn_id,
            session,
            false,
            additional_history,
            None,
        )
    }

    pub(in crate::plan) fn new_ephemeral_until_tool_completion(
        ctx: Ctx,
        template: SingleAgentTemplate,
        input: String,
        turn_id: u64,
        session: CommonSession,
        additional_history: Vec<SessionMessage>,
        tool_name: impl Into<String>,
        output_formatter: ToolCompletionFormatter,
    ) -> Self {
        Self::new_ephemeral_until_tool_completion_with_images(
            ctx,
            template,
            input,
            Vec::new(),
            turn_id,
            session,
            additional_history,
            tool_name,
            output_formatter,
        )
    }

    pub(in crate::plan) fn new_ephemeral_until_tool_completion_with_images(
        ctx: Ctx,
        template: SingleAgentTemplate,
        input: String,
        images: Vec<String>,
        turn_id: u64,
        session: CommonSession,
        additional_history: Vec<SessionMessage>,
        tool_name: impl Into<String>,
        output_formatter: ToolCompletionFormatter,
    ) -> Self {
        Self::new_with_persistence(
            ctx,
            template,
            input,
            images,
            turn_id,
            session,
            false,
            additional_history,
            Some(TerminalToolCompletion {
                tool_name: tool_name.into(),
                output_formatter,
            }),
        )
    }

    fn new_with_persistence(
        ctx: Ctx,
        template: SingleAgentTemplate,
        input: String,
        images: Vec<String>,
        turn_id: u64,
        session: CommonSession,
        persist_session: bool,
        additional_history: Vec<SessionMessage>,
        terminal_tool_completion: Option<TerminalToolCompletion>,
    ) -> Self {
        let initial_message = SessionMessage::user_with_images(input.clone(), images.clone());
        Self {
            id: format!("single_agent-{}", wd_tools::uuid::v4()),
            ctx,
            template,
            input,
            images,
            turn_id,
            session,
            stage: SingleAgentStage::Memory,
            user_memory: Vec::new(),
            messages: Vec::new(),
            additional_history,
            unsaved_messages: vec![initial_message],
            final_output: String::new(),
            tool_iterations: 0,
            empty_model_retries: 0,
            task_sequence: 0,
            pending_tools: HashMap::new(),
            persist_session,
            terminal_tool_completion,
            pending_terminal_output: None,
            pending_todo_output: None,
            todo_plan: None,
            owns_active_turn: true,
            finish_on_drop: false,
        }
    }

    fn hook_context<'a>(&'a self, task_id: Option<&'a str>) -> SingleAgentHookContext<'a> {
        SingleAgentHookContext {
            ctx: &self.ctx,
            plan_id: &self.id,
            turn_id: self.turn_id,
            agent: &self.template.agent,
            task_id,
        }
    }

    async fn emit(&self, source: impl Into<String>, data: SessionEventData) -> anyhow::Result<()> {
        self.session
            .emit_agent(self.turn_id, self.template.agent.name.clone(), source, data)
    }

    fn task<Req: Send + 'static>(&mut self, ty: TaskType, req: Req) -> TaskRequest {
        self.task_sequence += 1;
        TaskReq {
            ctx: self.ctx.clone(),
            meta: TaskMeta {
                id: format!("single-agent-{}-{}", self.turn_id, self.task_sequence),
                ty,
                ..Default::default()
            },
            req,
        }
        .into_request()
    }

    async fn tool_task(&mut self, request: ToolRequest) -> anyhow::Result<TaskRequest> {
        let request = self
            .template
            .hook
            .on_tools_req(&self.hook_context(None), request)
            .await?;
        Ok(self.task(TaskType::Tool, request))
    }

    fn history_task(&mut self) -> TaskRequest {
        self.task(
            TaskType::Session,
            SessionRequest::Query {
                agent_id: self.template.agent.name.clone(),
                user_id: self.session.user_id(),
                session_id: self.template.agent.session_id.clone(),
                limit: None,
                offset: None,
            },
        )
    }

    fn memory_task(&mut self) -> TaskRequest {
        self.task(
            TaskType::Memory,
            UserMemoryRequest::Query {
                user_id: self.session.user_id(),
            },
        )
    }

    fn model_request(&self) -> CreateChatCompletionRequest {
        CreateChatCompletionRequest {
            model: self.template.model.model.clone(),
            messages: self.messages.clone(),
            stream: Some(true),
            max_completion_tokens: self.template.model.max_completion_tokens,
            temperature: self.template.model.temperature,
            tools: (!self.template.tool_definitions.is_empty())
                .then(|| self.template.tool_definitions.clone()),
            safety_identifier: Some(self.template.agent.user_id.clone()),
            ..Default::default()
        }
    }

    async fn next_model_task(&mut self) -> anyhow::Result<TaskRequest> {
        let pending = self.session.take_pending_inputs();
        self.append_user_inputs(pending).await?;

        let request = self.model_request();
        let estimated_tokens = estimated_tokens(&request);
        if estimated_tokens > self.template.model.trigger_compression_size {
            let request = self
                .template
                .hook
                .on_compression(&self.hook_context(None), request)
                .await?;
            self.emit(
                COMPRESSION_TASK_TYPE,
                SessionEventData::CompressionStarted {
                    estimated_tokens,
                    trigger_compression_size: self.template.model.trigger_compression_size,
                },
            )
            .await?;
            let content = serde_json::to_string(
                &request
                    .messages
                    .iter()
                    .filter(|message| !matches!(message, ChatCompletionRequestMessage::System(_)))
                    .collect::<Vec<_>>(),
            )?;
            self.stage = SingleAgentStage::Compression;
            return Ok(self.task(
                TaskType::Any(COMPRESSION_TASK_TYPE.to_string()),
                WorkflowActionRequest {
                    action: "compression".to_string(),
                    payload: serde_json::json!({
                        "text": content,
                        "model": self.template.model.model,
                    }),
                },
            ));
        }

        let request = self
            .template
            .hook
            .on_model_req(&self.hook_context(None), request)
            .await?;
        self.stage = SingleAgentStage::Model;
        Ok(self.task(TaskType::Model, request))
    }

    async fn apply_compression(&mut self, content: String) -> anyhow::Result<TaskRequest> {
        anyhow::ensure!(
            !content.trim().is_empty(),
            "compression runtime returned empty content"
        );
        let content = content.trim().to_string();
        self.emit(
            COMPRESSION_TASK_TYPE,
            SessionEventData::CompressionCompleted {
                content: content.clone(),
            },
        )
        .await?;
        self.messages
            .retain(|message| matches!(message, ChatCompletionRequestMessage::System(_)));
        self.messages.push(summary_chat_message(&content));
        self.unsaved_messages.push(SessionMessage::summary(content));

        let request = self.model_request();
        anyhow::ensure!(
            estimated_tokens(&request) <= self.template.model.trigger_compression_size,
            "compressed model request still exceeds trigger_compression_size ({})",
            self.template.model.trigger_compression_size
        );
        self.next_model_task().await
    }

    async fn save_task(&mut self) -> anyhow::Result<TaskRequest> {
        let messages = self
            .template
            .hook
            .on_save(&self.hook_context(None), self.unsaved_messages.clone())
            .await?;
        Ok(self.task(
            TaskType::Session,
            SessionRequest::Add {
                agent_id: self.template.agent.name.clone(),
                user_id: self.session.user_id(),
                session_id: self.template.agent.session_id.clone(),
                messages,
            },
        ))
    }

    async fn append_user_inputs(&mut self, inputs: Vec<SessionInputData>) -> anyhow::Result<()> {
        for input in inputs {
            let images = input.images.unwrap_or_default();
            let content =
                user_message_content(&input.text, &images, &self.template.model.image_detail)
                    .await?;
            self.emit(
                self.template.agent.user_id.clone(),
                SessionEventData::UserInput {
                    content: input.text.clone(),
                },
            )
            .await?;
            self.messages.push(ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage {
                    content,
                    ..Default::default()
                },
            ));
            self.unsaved_messages
                .push(SessionMessage::user_with_images(input.text, images));
        }
        Ok(())
    }

    async fn prepare_messages(
        &mut self,
        history: Vec<SessionMessage>,
        task_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.messages.clear();
        let prompt = prompt_with_user_memory(&self.template.prompt, &self.user_memory)?;
        let prompt = self
            .template
            .hook
            .on_prompt(&self.hook_context(task_id), prompt)
            .await?;
        if !prompt.is_empty() {
            self.messages.push(ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessage {
                    content: prompt.into(),
                    ..Default::default()
                },
            ));
        }

        let history_limit = self.template.model.history_turns.saturating_mul(2);
        let history_start = history.len().saturating_sub(history_limit);
        let summary_start = history
            .iter()
            .rposition(|message| message.role == SessionMessageRole::Summary)
            .unwrap_or(0);
        let start = history_start.max(summary_start);
        for message in &history[start..] {
            self.messages
                .push(session_message_to_chat(message, &self.template.model.image_detail).await?);
        }
        for message in &self.additional_history {
            self.messages
                .push(session_message_to_chat(message, &self.template.model.image_detail).await?);
        }
        self.messages.push(ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                content: user_message_content(
                    &self.input,
                    &self.images,
                    &self.template.model.image_detail,
                )
                .await?,
                ..Default::default()
            },
        ));
        Ok(())
    }
}

mod lifecycle;
mod response;
mod tool_call;

use tool_call::ToolCallAccumulator;

#[cfg(test)]
include!("tests.rs");
