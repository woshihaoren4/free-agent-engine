use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use fae_agent::{Ctx, ToolInvocation, ToolRequest, ToolResponse, Tools};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::{
    DEFAULT_CHANNEL, TODO, effective_tool_name, ok_json, parse_arguments, request_tool_name,
    unsupported_tool,
};
use crate::{default_fae_host, validate_path_segment};

#[derive(Debug)]
pub struct TodoTool {
    host_dir: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Default for TodoTool {
    fn default() -> Self {
        Self::with_host_dir(default_fae_host())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TodoState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_input: Option<String>,
    next_id: u64,
    todos: BTreeMap<u64, Todo>,
}

impl Default for TodoState {
    fn default() -> Self {
        Self {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    user_input: Option<String>,
    todos: Vec<Todo>,
}

#[derive(Debug, Serialize)]
struct TodoResult {
    todo: Todo,
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
            "description": "Query, create, update, or delete durable todos for the current agent and user.",
            "parameters": {
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["query", "clear", "create", "update", "delete"],
                        "description": "Operation to perform. create requires contents; update requires id and at least one of content, completed, or assistant; delete requires id; query requires no other fields; clear optionally accepts user_input."
                    },
                    "id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Existing todo ID."
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
                        "description": "Original user input associated with a newly cleared todo plan."
                    }
                },
                "required": ["operation"],
                "additionalProperties": false
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, mut req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != TODO {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: TodoArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(response) => return Ok(response),
        };
        let Some(ToolInvocation::Todo { agent_id, user_id }) = req.take_invocation() else {
            return Ok(ToolResponse::with_error(
                400,
                "todo requires a single-agent context".to_string(),
            ));
        };
        let path = self.todo_path(&agent_id, &user_id)?;
        let _guard = self.lock.lock().await;
        let mut state = self.load_state(&path).await?;

        match args {
            TodoArgs::Query => ok_json(TodoList {
                user_input: state.user_input,
                todos: state.todos.values().cloned().collect(),
            }),
            TodoArgs::Clear { user_input } => {
                let user_input = match user_input
                    .map(|input| validate_text("user_input", input))
                    .transpose()
                {
                    Ok(user_input) => user_input,
                    Err(response) => return Ok(response),
                };
                state = TodoState {
                    user_input,
                    ..TodoState::default()
                };
                self.save_state(&path, &state).await?;
                ok_json(TodoList {
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
                self.save_state(&path, &state).await?;
                ok_json(TodoList {
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
                self.save_state(&path, &state).await?;
                ok_json(TodoResult { todo })
            }
            TodoArgs::Delete { id } => {
                if id == 0 {
                    return Ok(invalid_arguments("id must be positive"));
                }
                let Some(todo) = state.todos.remove(&id) else {
                    return Ok(not_found(id));
                };
                self.save_state(&path, &state).await?;
                ok_json(TodoResult { todo })
            }
        }
    }
}

impl TodoTool {
    pub fn with_host_dir(host_dir: impl Into<PathBuf>) -> Self {
        Self {
            host_dir: host_dir.into(),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn delete_state(&self, agent_id: &str, user_id: &str) -> anyhow::Result<bool> {
        let path = self.todo_path(agent_id, user_id)?;
        let _guard = self.lock.lock().await;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(anyhow::anyhow!("delete `{}`: {error}", path.display())),
        }
    }

    fn todo_path(&self, agent_id: &str, user_id: &str) -> anyhow::Result<PathBuf> {
        validate_path_segment("agent_id", agent_id).map_err(anyhow::Error::from)?;
        validate_path_segment("user_id", user_id).map_err(anyhow::Error::from)?;
        Ok(self
            .host_dir
            .join("session")
            .join(agent_id)
            .join(user_id)
            .join("todo.json"))
    }

    async fn load_state(&self, path: &Path) -> anyhow::Result<TodoState> {
        match tokio::fs::read(path).await {
            Ok(content) => serde_json::from_slice(&content)
                .map_err(|error| anyhow::anyhow!("parse todo file `{}`: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(TodoState::default()),
            Err(error) => Err(error)
                .map_err(|error| anyhow::anyhow!("read todo file `{}`: {error}", path.display())),
        }
    }

    async fn save_state(&self, path: &Path, state: &TodoState) -> anyhow::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("todo path has no parent: {}", path.display()))?;
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            anyhow::anyhow!("create todo directory `{}`: {error}", parent.display())
        })?;
        let content = serde_json::to_vec_pretty(state)?;
        tokio::fs::write(path, content)
            .await
            .map_err(|error| anyhow::anyhow!("write todo file `{}`: {error}", path.display()))
    }
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
    use fae_agent::{ContextNull, ToolRespItem};

    async fn execute(tool: &TodoTool, arguments: Value) -> Value {
        execute_for(tool, "agent-1", "user-1", arguments).await
    }

    async fn execute_for(
        tool: &TodoTool,
        agent_id: &str,
        user_id: &str,
        arguments: Value,
    ) -> Value {
        let ctx = Ctx::new(Arc::new(ContextNull));
        let mut response = tool
            .exec(
                &ctx,
                ToolRequest::new(TODO.to_string(), arguments.to_string()).with_invocation(
                    ToolInvocation::Todo {
                        agent_id: agent_id.to_string(),
                        user_id: user_id.to_string(),
                    },
                ),
            )
            .await
            .unwrap();
        let ToolRespItem::Completed(output) = response.next().await.unwrap() else {
            panic!("expected completed response");
        };
        serde_json::from_str(&output).unwrap()
    }

    fn temp_host(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fae-todo-tool-{name}-{}-{}",
            std::process::id(),
            wd_tools::uuid::v4()
        ))
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
                .join("todo.json")
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
