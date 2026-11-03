use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fae_agent::{Ctx, TodoPlanScope, ToolRequest, ToolResponse, Tools};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    DEFAULT_CHANNEL, TODO, effective_tool_name, ok_json, parse_arguments, request_tool_name,
    unsupported_tool,
};
use crate::{default_fae_host, validate_path_segment};

#[derive(Debug)]
pub struct TodoTool {
    host_dir: PathBuf,
}

impl Default for TodoTool {
    fn default() -> Self {
        Self::with_host_dir(default_fae_host())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TodoState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_input: Option<String>,
    next_id: u64,
    todos: BTreeMap<u64, Todo>,
}

impl Default for TodoState {
    fn default() -> Self {
        Self {
            plan_id: None,
            user_input: None,
            next_id: 1,
            todos: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Todo {
    id: u64,
    content: String,
    completed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assistant: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum TodoArgs {
    Query,
    // Internal, idempotent orchestration setup; intentionally absent from the model schema.
    Initialize {
        user_input: Option<String>,
    },
    Clear {
        user_input: Option<String>,
    },
    Create {
        contents: Vec<String>,
    },
    Update {
        id: u64,
        content: Option<String>,
        completed: Option<bool>,
        assistant: Option<String>,
    },
    Delete {
        id: u64,
    },
}

#[derive(Debug, Serialize)]
struct TodoList {
    plan_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_input: Option<String>,
    todos: Vec<Todo>,
}

#[derive(Debug, Serialize)]
struct TodoResult {
    plan_id: String,
    todo: Todo,
}

#[derive(Debug)]
pub struct TodoCleanupGuard {
    user_dir: PathBuf,
    _lock: File,
}

#[async_trait::async_trait]
impl Tools for TodoTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != TODO {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": TODO,
            "description": "Manage durable todos owned by the current agent, user, and todo plan. Numeric IDs are local to that plan. Without a bound plan, only query is allowed and returns plans grouped by plan_id, with any legacy todos shown separately.",
            "parameters": {
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["query", "clear", "create", "update", "delete"],
                        "description": "Operation within the current plan. create requires contents; update requires id and at least one of content, completed, or assistant; delete requires id; query requires no other fields. clear only clears the bound plan and never reuses IDs."
                    },
                    "id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Existing todo ID within the bound plan."
                    },
                    "content": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Updated content for an existing todo."
                    },
                    "contents": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "minLength": 1
                        },
                        "minItems": 1,
                        "description": "Contents for multiple todos created atomically in array order."
                    },
                    "completed": {
                        "type": "boolean",
                        "description": "Whether the todo is completed."
                    },
                    "assistant": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Assistant output produced when completing the todo."
                    },
                    "user_input": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Optional description when clearing the bound plan."
                    }
                },
                "required": ["operation"],
                "additionalProperties": false
            }
        }))
    }

    async fn exec(&self, ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != TODO {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: TodoArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(response) => return Ok(response),
        };
        let scope = ctx.scope()?;
        let agent_id = scope
            .agent_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("todo requires agent ownership in context"))?;
        let user_id = &scope.user.user_id;
        let user_dir = self.user_dir(agent_id, user_id)?;
        let todo = scope.todo.clone();
        if let Some(todo) = &todo {
            anyhow::ensure!(
                todo.agent_id == *agent_id && todo.user_id == *user_id,
                "todo plan owner does not match context"
            );
            validate_path_segment("plan_id", &todo.plan_id)?;
        } else if !matches!(args, TodoArgs::Query) {
            return Ok(ToolResponse::with_error(
                400,
                "todo mutations require plan ownership in context".into(),
            ));
        }
        if ctx.state().current().is_terminal() {
            return Ok(ToolResponse::with_error(
                409,
                "todo context is terminal".into(),
            ));
        }
        let ctx = ctx.clone();
        tokio::task::spawn_blocking(move || {
            // Maintenance precedes the CRUD lock everywhere, including cleanup.
            if let Some(todo) = &todo {
                if matches!(args, TodoArgs::Initialize { .. } | TodoArgs::Create { .. }) {
                    todo.acquire_lease(|| Ok(Box::new(lock_maintenance(&user_dir, false)?)))?;
                }
            }
            let _maintenance = lock_maintenance(&user_dir, false)?;
            let _guard = lock_user(&user_dir)?;
            if ctx.state().current().is_terminal() {
                return Ok(ToolResponse::with_error(
                    409,
                    "todo context is terminal".into(),
                ));
            }
            match todo {
                Some(todo) => Self::exec_plan(&user_dir, &todo, args),
                None => Self::query_plans(&user_dir),
            }
        })
        .await?
    }
}

impl TodoTool {
    fn exec_plan(
        user_dir: &Path,
        scope: &TodoPlanScope,
        args: TodoArgs,
    ) -> anyhow::Result<ToolResponse> {
        let plan_id = scope.plan_id.clone();
        let path = user_dir.join("todo").join(format!("{plan_id}.json"));
        let loaded = Self::load_state(&path)?;
        if let Some(state) = &loaded {
            anyhow::ensure!(
                state.plan_id.as_deref() == Some(plan_id.as_str()),
                "todo file plan ownership does not match `{}`",
                path.display()
            );
        }
        let mut state = loaded.unwrap_or_else(|| TodoState {
            plan_id: Some(plan_id.clone()),
            user_input: (!scope.user_input.trim().is_empty()).then(|| scope.user_input.clone()),
            ..Default::default()
        });

        match args {
            TodoArgs::Query => ok_json(TodoList {
                plan_id,
                user_input: state.user_input,
                todos: state.todos.values().cloned().collect(),
            }),
            TodoArgs::Initialize { user_input } => {
                if user_input
                    .as_deref()
                    .is_some_and(|input| input != scope.user_input)
                {
                    return Ok(invalid_arguments(
                        "initial input must match the bound todo plan",
                    ));
                }
                Self::save_state(&path, &state)?;
                ok_json(TodoList {
                    plan_id,
                    user_input: state.user_input,
                    todos: state.todos.values().cloned().collect(),
                })
            }
            TodoArgs::Clear { user_input } => {
                let user_input = match user_input
                    .map(|input| validate_text("user_input", input))
                    .transpose()
                {
                    Ok(user_input) => user_input,
                    Err(response) => return Ok(response),
                };
                state.todos.clear();
                if user_input.is_some() {
                    state.user_input = user_input;
                }
                Self::save_state(&path, &state)?;
                ok_json(TodoList {
                    plan_id,
                    user_input: state.user_input,
                    todos: Vec::new(),
                })
            }
            TodoArgs::Create { contents } => {
                if contents.is_empty() {
                    return Ok(invalid_arguments("contents cannot be empty"));
                }
                let contents = match contents
                    .into_iter()
                    .map(validate_content)
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(contents) => contents,
                    Err(response) => return Ok(response),
                };
                let count = u64::try_from(contents.len())
                    .map_err(|_| anyhow::anyhow!("todo batch is too large"))?;
                let Some(next_id) = state.next_id.checked_add(count) else {
                    return Ok(ToolResponse::with_error(
                        500,
                        "todo ID space exhausted".to_string(),
                    ));
                };
                let todos = contents
                    .into_iter()
                    .enumerate()
                    .map(|(offset, content)| Todo {
                        id: state.next_id + offset as u64,
                        content,
                        completed: false,
                        assistant: None,
                    })
                    .collect::<Vec<_>>();
                state.next_id = next_id;
                state
                    .todos
                    .extend(todos.iter().cloned().map(|todo| (todo.id, todo)));
                Self::save_state(&path, &state)?;
                ok_json(TodoList {
                    plan_id,
                    user_input: state.user_input,
                    todos,
                })
            }
            TodoArgs::Update {
                id,
                content,
                completed,
                assistant,
            } => {
                if id == 0 {
                    return Ok(invalid_arguments("id must be positive"));
                }
                if content.is_none() && completed.is_none() && assistant.is_none() {
                    return Ok(invalid_arguments(
                        "update requires at least one of content, completed, or assistant",
                    ));
                }
                let content = match content.map(validate_content).transpose() {
                    Ok(content) => content,
                    Err(response) => return Ok(response),
                };
                let assistant = match assistant
                    .map(|content| validate_text("assistant", content))
                    .transpose()
                {
                    Ok(assistant) => assistant,
                    Err(response) => return Ok(response),
                };
                let Some(todo) = state.todos.get_mut(&id) else {
                    return Ok(not_found(id));
                };
                if assistant.is_some() && !completed.unwrap_or(todo.completed) {
                    return Ok(invalid_arguments(
                        "assistant can only be set on a completed todo",
                    ));
                }
                if let Some(content) = content {
                    todo.content = content;
                }
                if let Some(completed) = completed {
                    todo.completed = completed;
                    if !completed {
                        todo.assistant = None;
                    }
                }
                if let Some(assistant) = assistant {
                    todo.assistant = Some(assistant);
                }
                let todo = todo.clone();
                Self::save_state(&path, &state)?;
                ok_json(TodoResult { plan_id, todo })
            }
            TodoArgs::Delete { id } => {
                if id == 0 {
                    return Ok(invalid_arguments("id must be positive"));
                }
                let Some(todo) = state.todos.remove(&id) else {
                    return Ok(not_found(id));
                };
                Self::save_state(&path, &state)?;
                ok_json(TodoResult { plan_id, todo })
            }
        }
    }
    pub fn with_host_dir(host_dir: impl Into<PathBuf>) -> Self {
        Self {
            host_dir: host_dir.into(),
        }
    }

    /// Retain this guard until both history and todo cleanup have finished.
    pub async fn lock_for_cleanup(
        &self,
        agent_id: &str,
        user_id: &str,
    ) -> anyhow::Result<TodoCleanupGuard> {
        let user_dir = self.user_dir(agent_id, user_id)?;
        tokio::task::spawn_blocking(move || {
            let lock = lock_maintenance(&user_dir, true)?;
            Ok(TodoCleanupGuard {
                user_dir,
                _lock: lock,
            })
        })
        .await?
    }

    pub async fn delete_state(&self, agent_id: &str, user_id: &str) -> anyhow::Result<bool> {
        self.lock_for_cleanup(agent_id, user_id)
            .await?
            .delete_state()
            .await
    }

    fn user_dir(&self, agent_id: &str, user_id: &str) -> anyhow::Result<PathBuf> {
        validate_path_segment("agent_id", agent_id).map_err(anyhow::Error::from)?;
        validate_path_segment("user_id", user_id).map_err(anyhow::Error::from)?;
        Ok(self.host_dir.join("session").join(agent_id).join(user_id))
    }

    fn load_state(path: &Path) -> anyhow::Result<Option<TodoState>> {
        match fs::read(path) {
            Ok(content) => serde_json::from_slice(&content)
                .map(Some)
                .map_err(|error| anyhow::anyhow!("parse todo file `{}`: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .map_err(|error| anyhow::anyhow!("read todo file `{}`: {error}", path.display())),
        }
    }

    fn save_state(path: &Path, state: &TodoState) -> anyhow::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("todo path has no parent: {}", path.display()))?;
        fs::create_dir_all(parent).map_err(|error| {
            anyhow::anyhow!("create todo directory `{}`: {error}", parent.display())
        })?;
        let content = serde_json::to_vec_pretty(state)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&content)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .map_err(|error| anyhow::anyhow!("commit todo file `{}`: {error}", path.display()))?;
        Ok(())
    }

    fn query_plans(user_dir: &Path) -> anyhow::Result<ToolResponse> {
        let mut plans = Vec::new();
        let directory = user_dir.join("todo");
        match fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry?.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("json") {
                        continue;
                    }
                    let state = Self::load_state(&path)?
                        .ok_or_else(|| anyhow::anyhow!("todo file disappeared during query"))?;
                    let plan_id = state
                        .plan_id
                        .ok_or_else(|| anyhow::anyhow!("todo plan file is missing plan_id"))?;
                    anyhow::ensure!(
                        path.file_stem().and_then(|value| value.to_str()) == Some(plan_id.as_str()),
                        "todo file plan ownership does not match `{}`",
                        path.display()
                    );
                    plans.push(TodoList {
                        plan_id,
                        user_input: state.user_input,
                        todos: state.todos.into_values().collect(),
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        plans.sort_by(|a, b| a.plan_id.cmp(&b.plan_id));
        let mut result = json!({ "plans": plans });
        if let Some(legacy) = Self::load_state(&user_dir.join("todo.json"))? {
            result["legacy"] = json!({
                "read_only": true,
                "user_input": legacy.user_input,
                "todos": legacy.todos.into_values().collect::<Vec<_>>(),
            });
        }
        ok_json(result)
    }
}

impl TodoCleanupGuard {
    pub async fn delete_state(&self) -> anyhow::Result<bool> {
        let user_dir = self.user_dir.clone();
        tokio::task::spawn_blocking(move || {
            let mut deleted = false;
            for path in [user_dir.join("todo"), user_dir.join("todo.json")] {
                let result = if path.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
                match result {
                    Ok(()) => deleted = true,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(anyhow::anyhow!("delete `{}`: {error}", path.display()));
                    }
                }
            }
            Ok(deleted)
        })
        .await?
    }
}

fn lock_user(user_dir: &Path) -> anyhow::Result<File> {
    fs::create_dir_all(user_dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(user_dir.join(".todo.lock"))?;
    lock.lock()?;
    Ok(lock)
}

fn lock_maintenance(user_dir: &Path, cleanup: bool) -> anyhow::Result<File> {
    fs::create_dir_all(user_dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(user_dir.join(".todo-maintenance.lock"))?;
    if cleanup {
        lock.try_lock().map_err(|error| {
            anyhow::anyhow!("todo cleanup is busy; finish active todo plans and retry: {error}")
        })?;
    } else {
        lock.lock_shared()?;
    }
    Ok(lock)
}

fn validate_content(content: String) -> Result<String, ToolResponse> {
    validate_text("content", content)
}

fn validate_text(field: &str, content: String) -> Result<String, ToolResponse> {
    let content = content.trim();
    if content.is_empty() {
        return Err(invalid_arguments(&format!("{field} cannot be empty")));
    }
    Ok(content.to_string())
}

fn invalid_arguments(message: &str) -> ToolResponse {
    ToolResponse::with_error(400, message.to_string())
}

fn not_found(id: u64) -> ToolResponse {
    ToolResponse::with_error(404, format!("todo {id} was not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::{ContextScope, ToolRespItem};

    async fn execute_in_context(tool: &TodoTool, ctx: &Ctx, arguments: Value) -> Value {
        let mut response = tool
            .exec(ctx, ToolRequest::new(TODO.into(), arguments.to_string()))
            .await
            .unwrap();
        let ToolRespItem::Completed(output) = response.next().await.unwrap() else {
            panic!("expected completed response");
        };
        serde_json::from_str(&output).unwrap()
    }

    async fn plan_context(plan_id: &str) -> Ctx {
        let engine = crate::EngineBuilder::new().build().await;
        let mut scope = ContextScope::root("alice");
        scope.agent_id = Some("agent-a".into());
        let ctx = engine.ctx_with_scope(scope).unwrap();
        ctx.with_todo_plan(plan_id.into(), "original request".into())
            .unwrap()
    }

    async fn execute(tool: &TodoTool, arguments: Value) -> Value {
        execute_for(tool, "agent-1", "user-1", arguments).await
    }

    async fn execute_for(
        tool: &TodoTool,
        agent_id: &str,
        user_id: &str,
        arguments: Value,
    ) -> Value {
        let engine = crate::EngineBuilder::new().build().await;
        let mut scope = ContextScope::root(user_id);
        scope.agent_id = Some(agent_id.to_string());
        scope.todo = Some(std::sync::Arc::new(TodoPlanScope::new(
            "test-plan".into(),
            agent_id.into(),
            user_id.into(),
            String::new(),
        )));
        let ctx = engine.ctx_with_scope(scope).unwrap();
        let mut response = tool
            .exec(
                &ctx,
                ToolRequest::new(TODO.to_string(), arguments.to_string()),
            )
            .await
            .unwrap();
        let ToolRespItem::Completed(output) = response.next().await.unwrap() else {
            panic!("expected completed response");
        };
        let mut output: Value = serde_json::from_str(&output).unwrap();
        if output.get("code").is_none() {
            assert_eq!(output["plan_id"], "test-plan");
            output.as_object_mut().unwrap().remove("plan_id");
        }
        output
    }

    fn temp_host(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fae-todo-tool-{name}-{}-{}",
            std::process::id(),
            wd_tools::uuid::v4()
        ))
    }

    #[tokio::test]
    async fn concurrent_plans_do_not_clear_or_complete_each_others_todos() {
        let host = temp_host("concurrent-plans");
        let tool_a = TodoTool::with_host_dir(&host);
        let tool_b = TodoTool::with_host_dir(&host);
        let engine = crate::EngineBuilder::new().build().await;
        let mut scope_a = ContextScope::root("alice");
        scope_a.agent_id = Some("agent-a".into());
        scope_a.scope_id = "plan-a".into();
        scope_a.todo = Some(std::sync::Arc::new(TodoPlanScope::new(
            "plan-a".into(),
            "agent-a".into(),
            "alice".into(),
            "A".into(),
        )));
        let mut scope_b = scope_a.clone();
        scope_b.scope_id = "plan-b".into();
        scope_b.todo = Some(std::sync::Arc::new(TodoPlanScope::new(
            "plan-b".into(),
            "agent-a".into(),
            "alice".into(),
            "B".into(),
        )));
        let ctx_a = engine.ctx_with_scope(scope_a).unwrap();
        let ctx_b = engine.ctx_with_scope(scope_b).unwrap();
        async fn call(tool: &TodoTool, ctx: &Ctx, args: Value) -> Value {
            let mut response = tool
                .exec(ctx, ToolRequest::new(TODO.into(), args.to_string()))
                .await
                .unwrap();
            let ToolRespItem::Completed(output) = response.next().await.unwrap() else {
                panic!("expected completed response");
            };
            serde_json::from_str(&output).unwrap()
        }
        call(&tool_a, &ctx_a, json!({"operation": "clear"})).await;
        call(
            &tool_a,
            &ctx_a,
            json!({"operation": "create", "contents": ["A task"]}),
        )
        .await;
        call(&tool_b, &ctx_b, json!({"operation": "clear"})).await;
        call(
            &tool_b,
            &ctx_b,
            json!({"operation": "create", "contents": ["B task"]}),
        )
        .await;
        call(
            &tool_a,
            &ctx_a,
            json!({"operation": "update", "id": 1, "completed": true, "assistant": "A result"}),
        )
        .await;
        let a = call(&tool_a, &ctx_a, json!({"operation": "query"})).await;
        let b = call(&tool_b, &ctx_b, json!({"operation": "query"})).await;
        let _ = tokio::fs::remove_dir_all(host).await;
        assert_eq!(a["todos"][0]["content"], "A task");
        assert_eq!(a["plan_id"], "plan-a");
        assert_eq!(b["plan_id"], "plan-b");
        assert_eq!(a["todos"][0]["assistant"], "A result");
        assert_eq!(b["todos"][0]["content"], "B task");
        assert_eq!(b["todos"][0]["completed"], false);
        assert!(b["todos"][0].get("assistant").is_none());
    }

    #[tokio::test]
    async fn initialization_is_idempotent_and_clear_does_not_reuse_ids() {
        let host = temp_host("initialize");
        let tool = TodoTool::with_host_dir(&host);
        let ctx = plan_context("plan-1").await;
        execute_in_context(
            &tool,
            &ctx,
            json!({"operation":"initialize","user_input":"original request"}),
        )
        .await;
        execute_in_context(
            &tool,
            &ctx,
            json!({"operation":"create","contents":["task"]}),
        )
        .await;
        let reset = execute_in_context(
            &tool,
            &ctx,
            json!({"operation":"initialize","user_input":"original request"}),
        )
        .await;
        assert_eq!(reset["todos"][0]["content"], "task");
        let conflict = execute_in_context(
            &tool,
            &ctx,
            json!({"operation":"initialize","user_input":"different"}),
        )
        .await;
        assert_eq!(conflict["code"], 400);
        execute_in_context(&tool, &ctx, json!({"operation":"clear"})).await;
        let created = execute_in_context(
            &tool,
            &ctx,
            json!({"operation":"create","contents":["next task"]}),
        )
        .await;
        assert_eq!(created["todos"][0]["id"], 2);
        assert_eq!(
            execute_in_context(
                &tool,
                &ctx,
                json!({"operation":"update","id":1,"completed":true})
            )
            .await["code"],
            404
        );
        drop(ctx);
        tokio::fs::remove_dir_all(host).await.unwrap();
    }

    #[tokio::test]
    async fn query_groups_plans_and_preserves_read_only_legacy_data() {
        let host = temp_host("groups");
        let tool = TodoTool::with_host_dir(&host);
        let a = plan_context("plan-a").await;
        let b = plan_context("plan-b").await;
        execute_in_context(&tool, &a, json!({"operation":"create","contents":["A"]})).await;
        execute_in_context(&tool, &b, json!({"operation":"create","contents":["B"]})).await;
        let user_dir = host.join("session/agent-a/alice");
        let legacy = r#"{"next_id":2,"todos":{"1":{"id":1,"content":"legacy","completed":false}}}"#;
        tokio::fs::write(user_dir.join("todo.json"), legacy)
            .await
            .unwrap();
        let root = crate::EngineBuilder::new().build().await;
        let mut scope = ContextScope::root("alice");
        scope.agent_id = Some("agent-a".into());
        let ctx = root.ctx_with_scope(scope).unwrap();
        let output = execute_in_context(&tool, &ctx, json!({"operation":"query"})).await;
        assert_eq!(output["plans"][0]["plan_id"], "plan-a");
        assert_eq!(output["plans"][1]["plan_id"], "plan-b");
        assert_eq!(output["legacy"]["read_only"], true);
        assert_eq!(output["legacy"]["todos"][0]["content"], "legacy");
        for args in [
            json!({"operation":"create","contents":["wrong"]}),
            json!({"operation":"clear"}),
            json!({"operation":"update","id":1,"completed":true}),
            json!({"operation":"delete","id":1}),
        ] {
            assert_eq!(execute_in_context(&tool, &ctx, args).await["code"], 400);
        }
        assert_eq!(
            tokio::fs::read_to_string(user_dir.join("todo.json"))
                .await
                .unwrap(),
            legacy
        );
        assert!(
            tool.delete_state("agent-a", "alice")
                .await
                .unwrap_err()
                .to_string()
                .contains("busy")
        );
        drop(a);
        drop(b);
        assert!(tool.delete_state("agent-a", "alice").await.unwrap());
        assert!(!user_dir.join("todo").exists());
        assert!(!user_dir.join("todo.json").exists());
        tokio::fs::remove_dir_all(host).await.unwrap();
    }

    #[tokio::test]
    async fn separate_instances_commit_concurrent_creates_without_lost_updates() {
        let host = temp_host("instances");
        let ctx = plan_context("shared-plan").await;
        let mut tasks = Vec::new();
        for index in 0..16 {
            let tool = TodoTool::with_host_dir(&host);
            let ctx = ctx.clone();
            tasks.push(tokio::spawn(async move {
                execute_in_context(
                    &tool,
                    &ctx,
                    json!({"operation":"create","contents":[format!("task-{index}")]}),
                )
                .await
            }));
        }
        let mut ids = Vec::new();
        for task in tasks {
            ids.push(task.await.unwrap()["todos"][0]["id"].as_u64().unwrap());
        }
        ids.sort();
        assert_eq!(ids, (1..=16).collect::<Vec<_>>());
        let query = execute_in_context(
            &TodoTool::with_host_dir(&host),
            &ctx,
            json!({"operation":"query"}),
        )
        .await;
        assert_eq!(query["todos"].as_array().unwrap().len(), 16);
        drop(ctx);
        tokio::fs::remove_dir_all(host).await.unwrap();
    }

    #[tokio::test]
    async fn corrupt_or_mismatched_plan_files_are_not_overwritten() {
        let host = temp_host("corrupt");
        let tool = TodoTool::with_host_dir(&host);
        let ctx = plan_context("plan-1").await;
        let path = host.join("session/agent-a/alice/todo/plan-1.json");
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        for original in ["{broken", r#"{"plan_id":"other","next_id":1,"todos":{}}"#] {
            tokio::fs::write(&path, original).await.unwrap();
            assert!(
                tool.exec(
                    &ctx,
                    ToolRequest::new(
                        TODO.into(),
                        json!({"operation":"create","contents":["new"]}).to_string()
                    )
                )
                .await
                .is_err()
            );
            assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), original);
        }
        ctx.set_state(fae_agent::ContextState::cancelled("stop"))
            .unwrap();
        assert_eq!(
            execute_in_context(
                &tool,
                &ctx,
                json!({"operation":"create","contents":["late"]})
            )
            .await["code"],
            409
        );
        drop(ctx);
        tokio::fs::remove_dir_all(host).await.unwrap();
    }

    #[test]
    fn file_lock_is_visible_to_another_process() {
        let host = temp_host("process-lock");
        let guard = lock_user(&host).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tools::todo::tests::checks_parent_process_lock",
                "--nocapture",
            ])
            .env("FAE_TODO_LOCK_TEST_PATH", &host)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        drop(guard);
        fs::remove_dir_all(host).unwrap();
    }

    #[test]
    fn checks_parent_process_lock() {
        let Some(path) = std::env::var_os("FAE_TODO_LOCK_TEST_PATH") else {
            return;
        };
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(PathBuf::from(path).join(".todo.lock"))
            .unwrap();
        assert!(lock.try_lock().is_err());
    }

    #[tokio::test]
    async fn supports_crud() {
        let host = temp_host("crud");
        let tool = TodoTool::with_host_dir(&host);

        let created = execute(
            &tool,
            json!({"operation": "create", "contents": ["  ship release  "]}),
        )
        .await;
        assert_eq!(
            created,
            json!({
                "todos": [{
                    "id": 1,
                    "content": "ship release",
                    "completed": false
                }]
            })
        );
        assert_eq!(
            execute(
                &tool,
                json!({
                    "operation": "update",
                    "id": 1,
                    "assistant": "premature result"
                })
            )
            .await["code"],
            400
        );

        let updated = execute(
            &tool,
            json!({
                "operation": "update",
                "id": 1,
                "content": "ship stable release",
                "completed": true,
                "assistant": "  release shipped successfully  "
            }),
        )
        .await;
        assert_eq!(updated["todo"]["content"], "ship stable release");
        assert_eq!(updated["todo"]["completed"], true);
        assert_eq!(updated["todo"]["assistant"], "release shipped successfully");

        let queried = execute(&tool, json!({"operation": "query"})).await;
        assert_eq!(queried["todos"].as_array().unwrap().len(), 1);
        assert_eq!(queried["todos"][0]["id"], 1);

        let reloaded = TodoTool::with_host_dir(&host);
        let queried = execute(&reloaded, json!({"operation": "query"})).await;
        assert_eq!(queried["todos"][0]["content"], "ship stable release");
        assert_eq!(
            queried["todos"][0]["assistant"],
            "release shipped successfully"
        );
        assert!(
            host.join("session")
                .join("agent-1")
                .join("user-1")
                .join("todo")
                .join("test-plan.json")
                .is_file()
        );

        let deleted = execute(&reloaded, json!({"operation": "delete", "id": 1})).await;
        assert_eq!(deleted["todo"]["id"], 1);
        assert_eq!(
            execute(&reloaded, json!({"operation": "query"})).await,
            json!({"todos": []})
        );
        let _ = tokio::fs::remove_dir_all(host).await;
    }

    #[tokio::test]
    async fn rejects_invalid_updates_and_missing_todos() {
        let host = temp_host("invalid");
        let tool = TodoTool::with_host_dir(&host);

        assert_eq!(
            execute(
                &tool,
                json!({"operation": "update", "id": 1, "content": null})
            )
            .await["code"],
            400
        );
        execute(
            &tool,
            json!({"operation": "create", "contents": ["pending task"]}),
        )
        .await;
        assert_eq!(
            execute(
                &tool,
                json!({
                    "operation": "update",
                    "id": 1,
                    "completed": false,
                    "assistant": "not completed"
                })
            )
            .await["code"],
            400
        );
        assert_eq!(
            execute(&tool, json!({"operation": "delete", "id": 2})).await["code"],
            404
        );
        assert_eq!(
            execute(&tool, json!({"operation": "create", "contents": ["  "]})).await["code"],
            400
        );
        let _ = tokio::fs::remove_dir_all(host).await;
    }

    #[tokio::test]
    async fn creates_multiple_todos_atomically() {
        let host = temp_host("batch-create");
        let tool = TodoTool::with_host_dir(&host);

        assert_eq!(
            execute(
                &tool,
                json!({
                    "operation": "create",
                    "contents": ["  inspect code  ", "implement change", "run tests"]
                })
            )
            .await,
            json!({
                "todos": [
                    {"id": 1, "content": "inspect code", "completed": false},
                    {"id": 2, "content": "implement change", "completed": false},
                    {"id": 3, "content": "run tests", "completed": false}
                ]
            })
        );
        assert_eq!(
            execute(
                &tool,
                json!({"operation": "create", "contents": ["report result"]})
            )
            .await["todos"][0]["id"],
            4
        );

        let _ = tokio::fs::remove_dir_all(host).await;
    }

    #[tokio::test]
    async fn rejects_invalid_batches_without_creating_todos() {
        let host = temp_host("invalid-batch");
        let tool = TodoTool::with_host_dir(&host);

        for arguments in [
            json!({"operation": "create", "contents": []}),
            json!({"operation": "create", "contents": ["valid", "  "]}),
            json!({"operation": "create", "content": "single"}),
            json!({"operation": "create"}),
        ] {
            assert_eq!(execute(&tool, arguments).await["code"], 400);
        }
        assert_eq!(
            execute(&tool, json!({"operation": "query"})).await,
            json!({"todos": []})
        );

        let _ = tokio::fs::remove_dir_all(host).await;
    }

    #[tokio::test]
    async fn isolates_todos_by_agent_and_user() {
        let host = temp_host("isolation");
        let tool = TodoTool::with_host_dir(&host);

        execute_for(
            &tool,
            "agent-a",
            "alice",
            json!({"operation": "create", "contents": ["alice task"]}),
        )
        .await;

        assert_eq!(
            execute_for(&tool, "agent-a", "bob", json!({"operation": "query"})).await,
            json!({"todos": []})
        );
        assert_eq!(
            execute_for(&tool, "agent-b", "alice", json!({"operation": "query"})).await,
            json!({"todos": []})
        );
        let _ = tokio::fs::remove_dir_all(host).await;
    }

    #[tokio::test]
    async fn clear_replaces_existing_todos_with_an_empty_plan() {
        let host = temp_host("clear");
        let tool = TodoTool::with_host_dir(&host);
        execute(
            &tool,
            json!({"operation": "create", "contents": ["stale task"]}),
        )
        .await;

        assert_eq!(
            execute(
                &tool,
                json!({
                    "operation": "clear",
                    "user_input": "  ship release  "
                })
            )
            .await,
            json!({"user_input": "ship release", "todos": []})
        );
        assert_eq!(
            execute(&tool, json!({"operation": "query"})).await,
            json!({"user_input": "ship release", "todos": []})
        );

        let reloaded = TodoTool::with_host_dir(&host);
        assert_eq!(
            execute(&reloaded, json!({"operation": "query"})).await,
            json!({"user_input": "ship release", "todos": []})
        );
        let _ = tokio::fs::remove_dir_all(host).await;
    }
}
