use async_openai::types::chat::ChatCompletionTools;
use serde::{Deserialize, Serialize};

use super::single_agent::{CallableRoute, CommonSession, SingleAgentPlan, SingleAgentTemplate};
use crate::{
    Ctx, Plan, PlanNext, SessionEventData, SessionMessage, SessionRequest, SessionResponse,
    SingleAgentHookContext, TaskMeta, TaskReq, TaskRequest, TaskResp, TaskResponse, TaskType,
    ToolInvocation, ToolRequest, ToolRespItem, ToolResponse,
};

const TODO_TOOL_NAME: &str = "todo";
const MAX_UNCHANGED_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SingleAgentMode {
    Todo,
}

impl SingleAgentMode {
    pub(super) fn parse(mode: Option<&str>) -> anyhow::Result<Option<Self>> {
        match mode.map(str::trim) {
            None | Some("") => Ok(None),
            Some("todo") => Ok(Some(Self::Todo)),
            Some(mode) => anyhow::bail!("unsupported single-agent mode `{mode}`"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TodoPhase {
    Planning,
    Executing,
    Summarizing,
}

#[derive(Debug)]
struct SingleAgentTodo {
    requirement: String,
    phase: TodoPhase,
    current_todo_id: Option<u64>,
    unchanged_attempts: usize,
    chat_history: Vec<SessionMessage>,
}

#[derive(Debug, Deserialize, Serialize)]
struct TodoList {
    todos: Vec<TodoItem>,
}

#[derive(Debug, Deserialize, Serialize)]
struct TodoItem {
    id: u64,
    content: String,
    completed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assistant: Option<String>,
}

fn format_created_todos(output: &str) -> anyhow::Result<String> {
    let list: TodoList = serde_json::from_str(output)
        .map_err(|error| anyhow::anyhow!("parse todo create response: {error}"))?;
    anyhow::ensure!(
        !list.todos.is_empty(),
        "todo create response contains no todos"
    );
    let items = list
        .todos
        .iter()
        .enumerate()
        .map(|(index, todo)| format!("{}. {}", index + 1, todo.content))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!("TODO create success:\n{items}"))
}

impl SingleAgentTodo {
    fn new(requirement: String) -> Self {
        Self {
            requirement,
            phase: TodoPhase::Planning,
            current_todo_id: None,
            unchanged_attempts: 0,
            chat_history: Vec::new(),
        }
    }

    fn planning_prompt(&self) -> String {
        format!(
            "User requirement: {}\n\n\
             You must first think through the requirement and use the todo tool to create a \
             complete execution plan in one call. You may use other available tools to gather \
             context before creating the plan, but do not execute any part of the requirement yet. \
             Finish immediately after the plan is created.",
            self.requirement
        )
    }

    fn next_prompt(&mut self, output: &str) -> anyhow::Result<String> {
        let list: TodoList = serde_json::from_str(output)
            .map_err(|error| anyhow::anyhow!("parse todo query response: {error}"))?;
        anyhow::ensure!(
            !list.todos.is_empty(),
            "todo mode requires the planning step to create at least one todo"
        );
        if self.phase == TodoPhase::Planning {
            anyhow::ensure!(
                list.todos.iter().any(|todo| !todo.completed),
                "todo mode requires the planning step to create an incomplete todo"
            );
        }

        if list.todos.iter().all(|todo| todo.completed) {
            self.phase = TodoPhase::Summarizing;
            self.current_todo_id = None;
            return Ok(self.summary_prompt());
        }

        let next = list
            .todos
            .iter()
            .find(|todo| !todo.completed)
            .expect("incomplete todo exists");
        if self.phase == TodoPhase::Executing && self.current_todo_id == Some(next.id) {
            self.unchanged_attempts += 1;
            anyhow::ensure!(
                self.unchanged_attempts <= MAX_UNCHANGED_ATTEMPTS,
                "todo {} remained incomplete after {} retries",
                next.id,
                MAX_UNCHANGED_ATTEMPTS
            );
        } else {
            self.unchanged_attempts = 0;
        }
        self.phase = TodoPhase::Executing;
        self.current_todo_id = Some(next.id);
        Ok(self.execution_prompt(&list, next))
    }

    fn execution_prompt(&self, list: &TodoList, next: &TodoItem) -> String {
        let task_number = list
            .todos
            .iter()
            .position(|todo| todo.id == next.id)
            .map(|position| position + 1)
            .expect("current todo belongs to the queried list");
        if task_number == 1 {
            format!(
                "The todo list has been generated. Complete the first task now: {}. Work only on \
                 this task and finish after it succeeds. Todo state is managed by the orchestrator.",
                next.content
            )
        } else {
            format!(
                "Now complete task {task_number}: {}. Work only on this task and finish after it \
                 succeeds. Todo state is managed by the orchestrator.",
                next.content
            )
        }
    }

    fn summary_prompt(&self) -> String {
        "All todos are complete. Summarize the completed work and provide the final concise \
         response to the user. Do not perform more work."
            .to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TodoPlanStage {
    Reset,
    RunningAgent,
    CompleteTodo,
    Query,
    Save,
}

#[derive(Debug)]
pub(super) struct SingleAgentTodoPlan {
    id: String,
    ctx: Ctx,
    template: SingleAgentTemplate,
    turn_id: u64,
    session: CommonSession,
    todo: SingleAgentTodo,
    stage: TodoPlanStage,
    child: Option<SingleAgentPlan>,
    child_session: Option<CommonSession>,
    current_prompt: Option<String>,
    final_output: Option<String>,
    task_sequence: u64,
    owns_active_turn: bool,
    finish_on_drop: bool,
}

impl SingleAgentTodoPlan {
    pub(super) fn new(
        ctx: Ctx,
        template: SingleAgentTemplate,
        input: String,
        turn_id: u64,
        session: CommonSession,
    ) -> Self {
        Self {
            id: format!("single_agent_todo-{}", wd_tools::uuid::v4()),
            ctx,
            template,
            turn_id,
            session,
            todo: SingleAgentTodo::new(input),
            stage: TodoPlanStage::Reset,
            child: None,
            child_session: None,
            current_prompt: None,
            final_output: None,
            task_sequence: 0,
            owns_active_turn: true,
            finish_on_drop: false,
        }
    }

    pub(super) async fn resume_from_created_todos(
        ctx: Ctx,
        template: SingleAgentTemplate,
        input: String,
        turn_id: u64,
        session: CommonSession,
        output: String,
    ) -> anyhow::Result<(Self, PlanNext)> {
        let created_todos = format_created_todos(&output)?;
        let mut plan = Self::new(ctx, template, input.clone(), turn_id, session);
        plan.todo.chat_history.extend([
            SessionMessage::user(input),
            SessionMessage::assistant(created_todos),
        ]);
        let prompt = plan.todo.next_prompt(&output)?;
        let next = plan.start_child(prompt).await?;
        Ok((plan, next))
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

    fn task<Req: Send + 'static>(&mut self, ty: TaskType, req: Req) -> TaskRequest {
        self.task_sequence += 1;
        TaskReq {
            ctx: self.ctx.clone(),
            meta: TaskMeta {
                id: format!("single-agent-todo-{}-{}", self.turn_id, self.task_sequence),
                ty,
                ..Default::default()
            },
            req,
        }
        .into_request()
    }

    async fn todo_task(&mut self, arguments: serde_json::Value) -> anyhow::Result<TaskRequest> {
        let runtime_tool_name = match self.template.tool_routes.get(TODO_TOOL_NAME) {
            Some(CallableRoute::Tool(runtime_tool_name)) => runtime_tool_name.clone(),
            _ => anyhow::bail!("single-agent todo mode requires the `todo` tool"),
        };
        let request = ToolRequest::new(runtime_tool_name, arguments.to_string()).with_invocation(
            ToolInvocation::Todo {
                agent_id: self.template.agent.name.clone(),
                user_id: self.session.user_id(),
            },
        );
        let request = self
            .template
            .hook
            .on_tools_req(&self.hook_context(None), request)
            .await?;
        Ok(self.task(TaskType::Tool, request))
    }

    async fn todo_output(
        &self,
        task_id: &str,
        task_result: &mut TaskResponse,
    ) -> anyhow::Result<String> {
        let response = TaskResp::<ToolResponse>::try_from_response(task_result)
            .ok_or_else(|| anyhow::anyhow!("expected ToolResponse from todo tool"))?;
        let mut response = self
            .template
            .hook
            .on_tools_resp(&self.hook_context(Some(task_id)), response.resp)
            .await?;
        loop {
            match response.next().await? {
                ToolRespItem::Streaming(_) => {}
                ToolRespItem::Completed(output) => return Ok(output),
            }
        }
    }

    fn child_template(&self) -> SingleAgentTemplate {
        let mut template = self.template.clone();
        match self.todo.phase {
            TodoPhase::Planning => {
                template.tool_definitions = template
                    .tool_definitions
                    .into_iter()
                    .map(planning_tool_definition)
                    .collect();
            }
            TodoPhase::Executing => {
                template
                    .tool_definitions
                    .retain(|definition| !is_todo_definition(definition));
                template.tool_routes.remove(TODO_TOOL_NAME);
            }
            TodoPhase::Summarizing => {
                template.tool_definitions.clear();
                template.tool_routes.clear();
            }
        }
        template
    }

    async fn start_child(&mut self, prompt: String) -> anyhow::Result<PlanNext> {
        let child_session = if self.todo.phase == TodoPhase::Summarizing {
            CommonSession::new_with_user_id(self.session.user_id())
        } else {
            CommonSession::new_in_agent(self.session.clone(), self.template.agent.name.clone())
        };
        let child_turn_id = child_session.activate_turn()?;
        let mut child = if self.todo.phase == TodoPhase::Planning {
            SingleAgentPlan::new_ephemeral_until_tool_completion(
                self.ctx.clone(),
                self.child_template(),
                prompt.clone(),
                child_turn_id,
                child_session.clone(),
                self.todo.chat_history.clone(),
                TODO_TOOL_NAME,
                format_created_todos,
            )
        } else {
            SingleAgentPlan::new_ephemeral(
                self.ctx.clone(),
                self.child_template(),
                prompt.clone(),
                child_turn_id,
                child_session.clone(),
                self.todo.chat_history.clone(),
            )
        };
        let next = child.init().await?;
        self.child = Some(child);
        self.child_session = Some(child_session);
        self.current_prompt = Some(prompt);
        self.stage = TodoPlanStage::RunningAgent;
        Ok(next)
    }

    async fn finish_child(&mut self) -> anyhow::Result<String> {
        self.child.take();
        let child_session = self
            .child_session
            .take()
            .expect("running todo agent requires a child session");
        let output = child_session.result().await?;
        let output = output
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("single-agent todo step returned non-string output"))?;
        let prompt = self
            .current_prompt
            .take()
            .ok_or_else(|| anyhow::anyhow!("single-agent todo step has no input prompt"))?;
        self.todo.chat_history.extend([
            SessionMessage::user(prompt),
            SessionMessage::assistant(output.clone()),
        ]);
        Ok(output)
    }

    async fn save_task(&mut self, content: String) -> anyhow::Result<PlanNext> {
        self.session.emit_agent(
            self.turn_id,
            self.template.agent.name.clone(),
            self.template.model.model.clone(),
            SessionEventData::ModelOutput {
                content: content.clone(),
            },
        )?;
        let messages = self
            .template
            .hook
            .on_save(
                &self.hook_context(None),
                vec![
                    SessionMessage::user(self.todo.requirement.clone()),
                    SessionMessage::assistant(content.clone()),
                ],
            )
            .await?;
        self.final_output = Some(content);
        self.stage = TodoPlanStage::Save;
        let request = SessionRequest::Add {
            agent_id: self.template.agent.name.clone(),
            user_id: self.session.user_id(),
            session_id: self.template.agent.session_id.clone(),
            messages,
        };
        Ok(PlanNext::Tasks(vec![self.task(TaskType::Session, request)]))
    }

    fn complete(&mut self) -> anyhow::Result<PlanNext> {
        let content = self
            .final_output
            .take()
            .ok_or_else(|| anyhow::anyhow!("todo mode completed without final output"))?;
        self.finish_on_drop = true;
        self.session.emit_agent(
            self.turn_id,
            self.template.agent.name.clone(),
            self.template.agent.name.clone(),
            SessionEventData::Completed { content },
        )?;
        Ok(PlanNext::End)
    }
}

fn planning_tool_definition(mut definition: ChatCompletionTools) -> ChatCompletionTools {
    match &mut definition {
        ChatCompletionTools::Function(tool) if tool.function.name == TODO_TOOL_NAME => {
            tool.function.description =
                Some("Create the complete execution plan as incomplete todos.".to_string());
            tool.function.parameters = Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["create"]
                    },
                    "contents": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "minLength": 1
                        },
                        "minItems": 1
                    }
                },
                "required": ["operation", "contents"],
                "additionalProperties": false
            }));
            tool.function.strict = Some(true);
        }
        _ => {}
    }
    definition
}

fn is_todo_definition(definition: &ChatCompletionTools) -> bool {
    matches!(
        definition,
        ChatCompletionTools::Function(tool) if tool.function.name == TODO_TOOL_NAME
    )
}

#[async_trait::async_trait]
impl Plan for SingleAgentTodoPlan {
    fn id(&self) -> &str {
        &self.id
    }

    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        if self.session.cancel_requested() {
            self.finish_on_drop = true;
            return Ok(PlanNext::End);
        }
        self.session.emit_agent(
            self.turn_id,
            self.template.agent.name.clone(),
            self.template.agent.name.clone(),
            SessionEventData::TurnStarted {
                input: self.todo.requirement.clone(),
            },
        )?;
        Ok(PlanNext::Tasks(vec![
            self.todo_task(serde_json::json!({
                "operation": "clear",
                "user_input": self.todo.requirement
            }))
            .await?,
        ]))
    }

    async fn next(&mut self, mut task_result: TaskResponse) -> anyhow::Result<PlanNext> {
        if self.session.cancel_requested() {
            self.child.take();
            self.child_session.take();
            self.finish_on_drop = true;
            return Ok(PlanNext::End);
        }

        match self.stage {
            TodoPlanStage::Reset => {
                let task_id = task_result.meta.id.clone();
                self.todo_output(&task_id, &mut task_result).await?;
                self.start_child(self.todo.planning_prompt()).await
            }
            TodoPlanStage::RunningAgent => {
                let next = self
                    .child
                    .as_mut()
                    .expect("running todo stage requires a child plan")
                    .next(task_result)
                    .await?;
                match next {
                    PlanNext::Tasks(tasks) => Ok(PlanNext::Tasks(tasks)),
                    PlanNext::End => {
                        let output = self.finish_child().await?;
                        if self.todo.phase == TodoPhase::Summarizing {
                            self.save_task(output).await
                        } else if self.todo.phase == TodoPhase::Executing {
                            let id = self.todo.current_todo_id.ok_or_else(|| {
                                anyhow::anyhow!("todo execution has no current ID")
                            })?;
                            self.stage = TodoPlanStage::CompleteTodo;
                            Ok(PlanNext::Tasks(vec![
                                self.todo_task(serde_json::json!({
                                    "operation": "update",
                                    "id": id,
                                    "completed": true,
                                    "assistant": output
                                }))
                                .await?,
                            ]))
                        } else {
                            self.stage = TodoPlanStage::Query;
                            Ok(PlanNext::Tasks(vec![
                                self.todo_task(serde_json::json!({ "operation": "query" }))
                                    .await?,
                            ]))
                        }
                    }
                }
            }
            TodoPlanStage::CompleteTodo => {
                let task_id = task_result.meta.id.clone();
                self.todo_output(&task_id, &mut task_result).await?;
                self.stage = TodoPlanStage::Query;
                Ok(PlanNext::Tasks(vec![
                    self.todo_task(serde_json::json!({ "operation": "query" }))
                        .await?,
                ]))
            }
            TodoPlanStage::Query => {
                let task_id = task_result.meta.id.clone();
                let output = self.todo_output(&task_id, &mut task_result).await?;
                let prompt = self.todo.next_prompt(&output)?;
                self.start_child(prompt).await
            }
            TodoPlanStage::Save => {
                let response = TaskResp::<SessionResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| {
                        anyhow::anyhow!("expected SessionResponse after todo session save")
                    })?;
                anyhow::ensure!(
                    matches!(response.resp, SessionResponse::Added { .. }),
                    "expected session add response"
                );
                self.complete()
            }
        }
    }

    async fn abort(&mut self, code: i32, error: String) {
        if let Some(child) = &mut self.child {
            child.abort(code, error.clone()).await;
        }
        self.child.take();
        self.child_session.take();
        self.session.abort_turn();
        self.owns_active_turn = false;
        let _ = self.session.emit_agent(
            self.turn_id,
            self.template.agent.name.clone(),
            self.template.agent.name.clone(),
            SessionEventData::Failed { error },
        );
    }
}

impl Drop for SingleAgentTodoPlan {
    fn drop(&mut self) {
        if self.owns_active_turn {
            if self.finish_on_drop {
                self.session.finish_turn();
            } else {
                self.session.abort_turn();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use async_openai::types::chat::{
        ChatCompletionTool, CreateChatCompletionRequest, CreateChatCompletionResponse,
        FunctionObject,
    };

    use super::*;
    use crate::{
        ModelResponse, Session, SessionResponse, SingleAgentHook, SingleAgentInfo,
        SingleAgentModelConfig, UserMemoryRequest, UserMemoryResponse,
    };

    #[derive(Debug)]
    struct TestHook;

    #[async_trait::async_trait]
    impl SingleAgentHook for TestHook {}

    fn tool_definition(name: &str) -> ChatCompletionTools {
        ChatCompletionTools::Function(ChatCompletionTool {
            function: FunctionObject {
                name: name.to_string(),
                description: None,
                parameters: None,
                strict: None,
            },
        })
    }

    fn test_template() -> SingleAgentTemplate {
        SingleAgentTemplate {
            agent: SingleAgentInfo {
                name: "test-agent".to_string(),
                desc: String::new(),
                user_id: "alice".to_string(),
                session_id: "session-1".to_string(),
                metadata: HashMap::new(),
            },
            prompt: "You are a test agent.".to_string(),
            model: SingleAgentModelConfig {
                model: "test-model".to_string(),
                trigger_compression_size: usize::MAX,
                history_turns: 10,
                max_completion_tokens: Some(1024),
                temperature: None,
                max_tool_iterations: 16,
            },
            tool_definitions: vec![
                tool_definition(TODO_TOOL_NAME),
                tool_definition("read_file"),
            ],
            tool_routes: HashMap::from([
                (
                    TODO_TOOL_NAME.to_string(),
                    CallableRoute::Tool(TODO_TOOL_NAME.to_string()),
                ),
                (
                    "read_file".to_string(),
                    CallableRoute::Tool("read_file".to_string()),
                ),
            ]),
            ancestor_agents: vec!["test-agent".to_string()],
            hook: Arc::new(TestHook),
        }
    }

    fn tool_response(ctx: &Ctx, output: &str) -> TaskResponse {
        TaskResp {
            ctx: ctx.clone(),
            meta: TaskMeta::default(),
            resp: ToolResponse::with_result(output.to_string()),
        }
        .into_response()
    }

    fn memory_response(ctx: &Ctx) -> TaskResponse {
        TaskResp {
            ctx: ctx.clone(),
            meta: TaskMeta::default(),
            resp: UserMemoryResponse::Memories {
                path: "memory.jsonl".into(),
                memories: Vec::new(),
            },
        }
        .into_response()
    }

    fn history_response(ctx: &Ctx) -> TaskResponse {
        TaskResp {
            ctx: ctx.clone(),
            meta: TaskMeta::default(),
            resp: SessionResponse::History {
                path: "session.jsonl".into(),
                messages: Vec::new(),
            },
        }
        .into_response()
    }

    fn model_response(ctx: &Ctx, content: &str) -> TaskResponse {
        let response: CreateChatCompletionResponse = serde_json::from_value(serde_json::json!({
            "id": "response-1",
            "choices": [{
                "index": 0,
                "message": {"content": content, "role": "assistant"},
                "finish_reason": "stop"
            }],
            "created": 0,
            "model": "test-model",
            "object": "chat.completion",
            "usage": null
        }))
        .unwrap();
        TaskResp {
            ctx: ctx.clone(),
            meta: TaskMeta::default(),
            resp: ModelResponse::Completed(response),
        }
        .into_response()
    }

    fn model_parallel_tool_response(
        ctx: &Ctx,
        calls: &[(&str, serde_json::Value)],
    ) -> TaskResponse {
        let tool_calls = calls
            .iter()
            .enumerate()
            .map(|(index, (tool_name, arguments))| {
                serde_json::json!({
                    "id": format!("call-{}", index + 1),
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": arguments.to_string()
                    }
                })
            })
            .collect::<Vec<_>>();
        let response: CreateChatCompletionResponse = serde_json::from_value(serde_json::json!({
            "id": "response-1",
            "choices": [{
                "index": 0,
                "message": {
                    "content": null,
                    "role": "assistant",
                    "tool_calls": tool_calls
                },
                "finish_reason": "tool_calls"
            }],
            "created": 0,
            "model": "test-model",
            "object": "chat.completion",
            "usage": null
        }))
        .unwrap();
        TaskResp {
            ctx: ctx.clone(),
            meta: TaskMeta::default(),
            resp: ModelResponse::Completed(response),
        }
        .into_response()
    }

    fn save_response(ctx: &Ctx) -> TaskResponse {
        TaskResp {
            ctx: ctx.clone(),
            meta: TaskMeta::default(),
            resp: SessionResponse::Added {
                path: "session.jsonl".into(),
                added: 2,
            },
        }
        .into_response()
    }

    async fn advance_to_model(
        plan: &mut SingleAgentTodoPlan,
        ctx: &Ctx,
    ) -> CreateChatCompletionRequest {
        let PlanNext::Tasks(mut tasks) = plan.next(memory_response(ctx)).await.unwrap() else {
            panic!("expected history task");
        };
        assert!(TaskReq::<UserMemoryRequest>::try_from_request(&mut tasks[0]).is_none());

        let PlanNext::Tasks(mut tasks) = plan.next(history_response(ctx)).await.unwrap() else {
            panic!("expected model task");
        };
        TaskReq::<CreateChatCompletionRequest>::try_from_request(&mut tasks[0])
            .unwrap()
            .req
    }

    fn conversation_messages(request: &CreateChatCompletionRequest) -> Vec<(String, String)> {
        request
            .messages
            .iter()
            .filter_map(|message| {
                let message = serde_json::to_value(message).unwrap();
                let role = message["role"].as_str()?;
                if role == "system" {
                    return None;
                }
                Some((
                    role.to_string(),
                    message["content"].as_str().unwrap_or_default().to_string(),
                ))
            })
            .collect()
    }

    #[test]
    fn progresses_from_planning_to_execution_and_summary() {
        let mut todo = SingleAgentTodo::new("ship it".to_string());
        assert_eq!(todo.phase, TodoPhase::Planning);

        let prompt = todo
            .next_prompt(r#"{"todos":[{"id":1,"content":"test","completed":false}]}"#)
            .unwrap();
        assert!(prompt.contains("Complete the first task now: test"));
        assert_eq!(todo.phase, TodoPhase::Executing);

        let prompt = todo
            .next_prompt(r#"{"todos":[{"id":1,"content":"test","completed":true}]}"#)
            .unwrap();
        assert!(prompt.contains("Summarize the completed work"));
        assert_eq!(todo.phase, TodoPhase::Summarizing);
    }

    #[test]
    fn rejects_an_empty_plan() {
        let mut todo = SingleAgentTodo::new("ship it".to_string());
        assert!(todo.next_prompt(r#"{"todos":[]}"#).is_err());
    }

    #[test]
    fn formats_created_todos_as_a_numbered_list() {
        assert_eq!(
            format_created_todos(
                r#"{"todos":[{"id":1,"content":"implement","completed":false},{"id":2,"content":"verify","completed":false}]}"#
            )
            .unwrap(),
            "TODO create success:\n1. implement\n2. verify"
        );
    }

    #[test]
    fn parses_public_mode_values() {
        assert_eq!(SingleAgentMode::parse(None).unwrap(), None);
        assert_eq!(SingleAgentMode::parse(Some("")).unwrap(), None);
        assert_eq!(
            SingleAgentMode::parse(Some("todo")).unwrap(),
            Some(SingleAgentMode::Todo)
        );
        assert!(SingleAgentMode::parse(Some("unknown")).is_err());
    }

    #[test]
    fn planning_tool_definition_only_restricts_todo_to_batch_creation() {
        let definition = planning_tool_definition(tool_definition(TODO_TOOL_NAME));
        let ChatCompletionTools::Function(tool) = definition else {
            panic!("expected function tool");
        };
        let parameters = tool.function.parameters.unwrap();

        assert_eq!(tool.function.strict, Some(true));
        assert_eq!(
            parameters["properties"]["operation"]["enum"],
            serde_json::json!(["create"])
        );
        assert_eq!(
            parameters["required"],
            serde_json::json!(["operation", "contents"])
        );
        assert!(parameters["properties"].get("id").is_none());
        assert!(parameters["properties"].get("completed").is_none());
        assert!(matches!(
            planning_tool_definition(tool_definition("read_file")),
            ChatCompletionTools::Function(tool) if tool.function.name == "read_file"
        ));
    }

    #[tokio::test]
    async fn orchestrates_separate_single_agent_plans_until_summary() {
        let ctx = Ctx::null();
        let session = CommonSession::new_with_user_id("alice");
        session.activate_turn().unwrap();
        let mut plan = SingleAgentTodoPlan::new(
            ctx.clone(),
            test_template(),
            "ship release".to_string(),
            1,
            session.clone(),
        );

        let PlanNext::Tasks(mut tasks) = plan.init().await.unwrap() else {
            panic!("expected reset task");
        };
        let mut reset = TaskReq::<ToolRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(reset.req.get_arguments()).unwrap(),
            serde_json::json!({
                "operation": "clear",
                "user_input": "ship release"
            })
        );
        assert!(matches!(
            reset.req.take_invocation(),
            Some(ToolInvocation::Todo { agent_id, user_id })
                if agent_id == "test-agent" && user_id == "alice"
        ));

        let PlanNext::Tasks(_) = plan
            .next(tool_response(&ctx, r#"{"todos":[]}"#))
            .await
            .unwrap()
        else {
            panic!("expected planning child");
        };
        let planning = advance_to_model(&mut plan, &ctx).await;
        let planning_messages = conversation_messages(&planning);
        assert_eq!(planning_messages.len(), 1);
        assert_eq!(planning_messages[0].0, "user");
        assert!(planning_messages[0].1.contains("ship release"));
        assert!(
            planning_messages[0]
                .1
                .contains("create a complete execution plan")
        );
        assert!(planning.tool_choice.is_none());
        assert!(planning.parallel_tool_calls.is_none());
        let planning_tools = planning.tools.unwrap();
        assert_eq!(planning_tools.len(), 2);
        assert!(planning_tools.iter().any(|definition| matches!(
            definition,
            ChatCompletionTools::Function(tool) if tool.function.name == TODO_TOOL_NAME
        )));
        assert!(planning_tools.iter().any(|definition| matches!(
            definition,
            ChatCompletionTools::Function(tool) if tool.function.name == "read_file"
        )));

        let PlanNext::Tasks(mut tasks) = plan
            .next(model_parallel_tool_response(
                &ctx,
                &[
                    ("read_file", serde_json::json!({ "path": "README.md" })),
                    ("read_file", serde_json::json!({ "path": "Cargo.toml" })),
                ],
            ))
            .await
            .unwrap()
        else {
            panic!("expected read_file calls");
        };
        assert_eq!(tasks.len(), 2);
        let read_first = TaskReq::<ToolRequest>::try_from_request(&mut tasks[0]).unwrap();
        let read_second = TaskReq::<ToolRequest>::try_from_request(&mut tasks[1]).unwrap();
        assert_eq!(read_first.req.get_tool_name(), "read_file");
        assert_eq!(read_second.req.get_tool_name(), "read_file");

        let PlanNext::Tasks(tasks) = plan
            .next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: read_first.meta,
                    resp: ToolResponse::with_result("README context".to_string()),
                }
                .into_response(),
            )
            .await
            .unwrap()
        else {
            panic!("expected pending read_file call");
        };
        assert!(tasks.is_empty());

        let PlanNext::Tasks(_) = plan
            .next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: read_second.meta,
                    resp: ToolResponse::with_result("Cargo context".to_string()),
                }
                .into_response(),
            )
            .await
            .unwrap()
        else {
            panic!("expected planning model after read_file calls");
        };

        let PlanNext::Tasks(mut tasks) = plan
            .next(model_parallel_tool_response(
                &ctx,
                &[
                    ("read_file", serde_json::json!({ "path": "src/lib.rs" })),
                    (
                        TODO_TOOL_NAME,
                        serde_json::json!({
                            "operation": "create",
                            "contents": ["implement", "verify"]
                        }),
                    ),
                ],
            ))
            .await
            .unwrap()
        else {
            panic!("expected final planning tool calls");
        };
        assert_eq!(tasks.len(), 2);
        let final_read = TaskReq::<ToolRequest>::try_from_request(&mut tasks[0]).unwrap();
        let create = TaskReq::<ToolRequest>::try_from_request(&mut tasks[1]).unwrap();
        assert_eq!(final_read.req.get_tool_name(), "read_file");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(create.req.get_arguments()).unwrap(),
            serde_json::json!({
                "operation": "create",
                "contents": ["implement", "verify"]
            })
        );

        let PlanNext::Tasks(tasks) = plan
            .next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: create.meta,
                    resp: ToolResponse::with_result(
                        r#"{"todos":[{"id":1,"content":"implement","completed":false},{"id":2,"content":"verify","completed":false}]}"#
                            .to_string(),
                    ),
                }
                .into_response(),
            )
            .await
            .unwrap()
        else {
            panic!("expected pending final read_file call");
        };
        assert!(tasks.is_empty());

        let PlanNext::Tasks(mut tasks) = plan
            .next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: final_read.meta,
                    resp: ToolResponse::with_result("final context".to_string()),
                }
                .into_response(),
            )
            .await
            .unwrap()
        else {
            panic!("expected todo query after create");
        };
        let query = TaskReq::<ToolRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert_eq!(query.req.get_arguments(), r#"{"operation":"query"}"#);

        let PlanNext::Tasks(_) = plan
            .next(tool_response(
                &ctx,
                r#"{"todos":[{"id":1,"content":"implement","completed":false},{"id":2,"content":"verify","completed":false}]}"#,
            ))
            .await
            .unwrap()
        else {
            panic!("expected execution child");
        };
        let execution = advance_to_model(&mut plan, &ctx).await;
        let execution_messages = conversation_messages(&execution);
        assert_eq!(execution_messages.len(), 3);
        assert_eq!(execution_messages[0], planning_messages[0]);
        assert_eq!(
            execution_messages[1],
            (
                "assistant".to_string(),
                "TODO create success:\n1. implement\n2. verify".to_string()
            )
        );
        assert_eq!(execution_messages[2].0, "user");
        assert!(
            execution_messages[2]
                .1
                .contains("Complete the first task now: implement")
        );
        let execution_tools = execution.tools.unwrap();
        assert_eq!(execution_tools.len(), 1);
        assert!(matches!(
            &execution_tools[0],
            ChatCompletionTools::Function(tool) if tool.function.name == "read_file"
        ));

        let PlanNext::Tasks(mut tasks) = plan
            .next(model_response(&ctx, "implemented"))
            .await
            .unwrap()
        else {
            panic!("expected todo completion update");
        };
        let mut update = TaskReq::<ToolRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(update.req.get_arguments()).unwrap(),
            serde_json::json!({
                "operation": "update",
                "id": 1,
                "completed": true,
                "assistant": "implemented"
            })
        );
        assert!(matches!(
            update.req.take_invocation(),
            Some(ToolInvocation::Todo { agent_id, user_id })
                if agent_id == "test-agent" && user_id == "alice"
        ));

        let PlanNext::Tasks(mut tasks) = plan
            .next(tool_response(
                &ctx,
                r#"{"todo":{"id":1,"content":"implement","completed":true,"assistant":"implemented"}}"#,
            ))
            .await
            .unwrap()
        else {
            panic!("expected todo query");
        };
        let query = TaskReq::<ToolRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert_eq!(query.req.get_arguments(), r#"{"operation":"query"}"#);

        let PlanNext::Tasks(_) = plan
            .next(tool_response(
                &ctx,
                r#"{"user_input":"ship release","todos":[{"id":1,"content":"implement","completed":true,"assistant":"implemented"},{"id":2,"content":"verify","completed":false}]}"#,
            ))
            .await
            .unwrap()
        else {
            panic!("expected second execution child");
        };
        let second_execution = advance_to_model(&mut plan, &ctx).await;
        let second_execution_messages = conversation_messages(&second_execution);
        assert_eq!(second_execution_messages.len(), 5);
        assert_eq!(&second_execution_messages[..3], execution_messages);
        assert_eq!(
            second_execution_messages[3],
            ("assistant".to_string(), "implemented".to_string())
        );
        assert_eq!(second_execution_messages[4].0, "user");
        assert!(
            second_execution_messages[4]
                .1
                .contains("Now complete task 2: verify")
        );
        assert_eq!(second_execution.tools.unwrap().len(), 1);

        let PlanNext::Tasks(mut tasks) = plan.next(model_response(&ctx, "verified")).await.unwrap()
        else {
            panic!("expected second todo completion update");
        };
        let update = TaskReq::<ToolRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(update.req.get_arguments()).unwrap(),
            serde_json::json!({
                "operation": "update",
                "id": 2,
                "completed": true,
                "assistant": "verified"
            })
        );

        let PlanNext::Tasks(_) = plan
            .next(tool_response(
                &ctx,
                r#"{"todo":{"id":2,"content":"verify","completed":true,"assistant":"verified"}}"#,
            ))
            .await
            .unwrap()
        else {
            panic!("expected todo query");
        };

        let PlanNext::Tasks(_) = plan
            .next(tool_response(
                &ctx,
                r#"{"user_input":"ship release","todos":[{"id":1,"content":"implement","completed":true,"assistant":"implemented"},{"id":2,"content":"verify","completed":true,"assistant":"verified"}]}"#,
            ))
            .await
            .unwrap()
        else {
            panic!("expected summary child");
        };
        let summary = advance_to_model(&mut plan, &ctx).await;
        let summary_messages = conversation_messages(&summary);
        assert_eq!(summary_messages.len(), 7);
        assert_eq!(&summary_messages[..5], second_execution_messages);
        assert_eq!(
            summary_messages[5],
            ("assistant".to_string(), "verified".to_string())
        );
        assert_eq!(summary_messages[6].0, "user");
        assert!(
            summary_messages[6]
                .1
                .contains("Summarize the completed work")
        );
        assert!(summary.tools.is_none());

        let PlanNext::Tasks(mut tasks) = plan
            .next(model_response(&ctx, "release shipped"))
            .await
            .unwrap()
        else {
            panic!("expected outer session save");
        };
        let save = TaskReq::<SessionRequest>::try_from_request(&mut tasks[0]).unwrap();
        let SessionRequest::Add {
            agent_id,
            user_id,
            session_id,
            messages,
        } = save.req
        else {
            panic!("expected session add request");
        };
        assert_eq!(agent_id, "test-agent");
        assert_eq!(user_id, "alice");
        assert_eq!(session_id, "session-1");
        assert_eq!(
            messages,
            vec![
                SessionMessage::user("ship release"),
                SessionMessage::assistant("release shipped"),
            ]
        );
        assert!(matches!(
            plan.next(save_response(&ctx)).await.unwrap(),
            PlanNext::End
        ));
        assert_eq!(
            session.result().await.unwrap(),
            serde_json::Value::String("release shipped".to_string())
        );

        let mut top_level_output = String::new();
        while let Some(event) = session.answer().await.unwrap() {
            let terminal = event.is_terminal();
            if event.parament_plan_id.is_none()
                && let SessionEventData::ModelOutput { content } = event.event_data().unwrap()
            {
                top_level_output.push_str(&content);
            }
            if terminal {
                break;
            }
        }
        assert_eq!(top_level_output, "release shipped");
    }
}
