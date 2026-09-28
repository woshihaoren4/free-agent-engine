use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use fae_agent::{Ctx, ToolRequest, ToolResponse, Tools};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    DEFAULT_CHANNEL, TODO, effective_tool_name, ok_json, parse_arguments, request_tool_name,
    unsupported_tool,
};

#[derive(Debug, Default)]
pub struct TodoTool {
    state: Arc<RwLock<TodoState>>,
}

#[derive(Debug)]
struct TodoState {
    next_id: u64,
    todos: BTreeMap<u64, Todo>,
}

impl Default for TodoState {
    fn default() -> Self {
        Self {
            next_id: 1,
            todos: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Todo {
    id: u64,
    content: String,
    completed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum TodoArgs {
    Query,
    Create {
        content: String,
    },
    Update {
        id: u64,
        content: Option<String>,
        completed: Option<bool>,
    },
    Delete {
        id: u64,
    },
}

#[derive(Debug, Serialize)]
struct TodoList {
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
            "description": "Query, create, update, or delete todos for the current engine instance.",
            "parameters": {
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["query", "create", "update", "delete"],
                        "description": "Operation to perform. create requires content; update requires id and at least one of content or completed; delete requires id; query requires no other fields."
                    },
                    "id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Existing todo ID."
                    },
                    "content": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Todo content."
                    },
                    "completed": {
                        "type": "boolean",
                        "description": "Whether the todo is completed."
                    }
                },
                "required": ["operation"],
                "additionalProperties": false
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != TODO {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: TodoArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(response) => return Ok(response),
        };

        match args {
            TodoArgs::Query => {
                let state = self.read_state()?;
                ok_json(TodoList {
                    todos: state.todos.values().cloned().collect(),
                })
            }
            TodoArgs::Create { content } => {
                let content = match validate_content(content) {
                    Ok(content) => content,
                    Err(response) => return Ok(response),
                };
                let mut state = self.write_state()?;
                let id = state.next_id;
                let Some(next_id) = id.checked_add(1) else {
                    return Ok(ToolResponse::with_error(
                        500,
                        "todo ID space exhausted".to_string(),
                    ));
                };
                state.next_id = next_id;
                let todo = Todo {
                    id,
                    content,
                    completed: false,
                };
                state.todos.insert(id, todo.clone());
                ok_json(TodoResult { todo })
            }
            TodoArgs::Update {
                id,
                content,
                completed,
            } => {
                if id == 0 {
                    return Ok(invalid_arguments("id must be positive"));
                }
                if content.is_none() && completed.is_none() {
                    return Ok(invalid_arguments(
                        "update requires at least one of content or completed",
                    ));
                }
                let content = match content.map(validate_content).transpose() {
                    Ok(content) => content,
                    Err(response) => return Ok(response),
                };
                let mut state = self.write_state()?;
                let Some(todo) = state.todos.get_mut(&id) else {
                    return Ok(not_found(id));
                };
                if let Some(content) = content {
                    todo.content = content;
                }
                if let Some(completed) = completed {
                    todo.completed = completed;
                }
                ok_json(TodoResult { todo: todo.clone() })
            }
            TodoArgs::Delete { id } => {
                if id == 0 {
                    return Ok(invalid_arguments("id must be positive"));
                }
                let mut state = self.write_state()?;
                let Some(todo) = state.todos.remove(&id) else {
                    return Ok(not_found(id));
                };
                ok_json(TodoResult { todo })
            }
        }
    }
}

impl TodoTool {
    fn read_state(&self) -> anyhow::Result<std::sync::RwLockReadGuard<'_, TodoState>> {
        self.state
            .read()
            .map_err(|_| anyhow::anyhow!("todo state lock is poisoned"))
    }

    fn write_state(&self) -> anyhow::Result<std::sync::RwLockWriteGuard<'_, TodoState>> {
        self.state
            .write()
            .map_err(|_| anyhow::anyhow!("todo state lock is poisoned"))
    }
}

fn validate_content(content: String) -> Result<String, ToolResponse> {
    let content = content.trim();
    if content.is_empty() {
        return Err(invalid_arguments("content cannot be empty"));
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
        let ctx = Ctx::new(Arc::new(ContextNull));
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
        serde_json::from_str(&output).unwrap()
    }

    #[tokio::test]
    async fn supports_crud() {
        let tool = TodoTool::default();

        let created = execute(
            &tool,
            json!({"operation": "create", "content": "  ship release  "}),
        )
        .await;
        assert_eq!(
            created,
            json!({
                "todo": {
                    "id": 1,
                    "content": "ship release",
                    "completed": false
                }
            })
        );

        let updated = execute(
            &tool,
            json!({
                "operation": "update",
                "id": 1,
                "content": "ship stable release",
                "completed": true
            }),
        )
        .await;
        assert_eq!(updated["todo"]["content"], "ship stable release");
        assert_eq!(updated["todo"]["completed"], true);

        let queried = execute(&tool, json!({"operation": "query"})).await;
        assert_eq!(queried["todos"].as_array().unwrap().len(), 1);
        assert_eq!(queried["todos"][0]["id"], 1);

        let deleted = execute(&tool, json!({"operation": "delete", "id": 1})).await;
        assert_eq!(deleted["todo"]["id"], 1);
        assert_eq!(
            execute(&tool, json!({"operation": "query"})).await,
            json!({"todos": []})
        );
    }

    #[tokio::test]
    async fn rejects_invalid_updates_and_missing_todos() {
        let tool = TodoTool::default();

        assert_eq!(
            execute(
                &tool,
                json!({"operation": "update", "id": 1, "content": null})
            )
            .await["code"],
            400
        );
        assert_eq!(
            execute(&tool, json!({"operation": "delete", "id": 1})).await["code"],
            404
        );
        assert_eq!(
            execute(&tool, json!({"operation": "create", "content": "  "})).await["code"],
            400
        );
    }
}
