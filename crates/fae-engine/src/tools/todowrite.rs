use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

pub struct TodoWrite {
    todos: RwLock<Vec<TodoItem>>,
}

impl Default for TodoWrite {
    fn default() -> Self {
        Self {
            todos: RwLock::new(Vec::new()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
    pub priority: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TodoWriteArgs {
    pub merge: bool,
    pub summary: Option<String>,
    pub todos: Vec<TodoItem>,
}

#[async_trait]
impl Tool for TodoWrite {
    fn name(&self) -> &str {
        "todo_write"
    }

    fn description(&self) -> &str {
        "Use this tool to create and manage a structured task list for your current coding session."
    }

    fn arguments(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "merge": {
                    "type": "boolean",
                    "description": "Whether to merge the todos with the existing todos. If true, the todos will be merged into the existing todos based on the id field."
                },
                "summary": {
                    "type": "string",
                    "description": "User-friendly summary of actual work accomplished when tasks are marked as completed."
                },
                "todos": {
                    "type": "array",
                    "description": "Array of todo items to write to the workspace",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Unique identifier for the todo item"
                            },
                            "content": {
                                "type": "string",
                                "description": "The description/content of the todo item."
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "The current status of the todo item"
                            },
                            "priority": {
                                "type": "string",
                                "enum": ["high", "medium", "low"]
                            }
                        },
                        "required": ["id", "content", "status", "priority"]
                    }
                }
            },
            "required": ["merge", "todos"]
        })
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<String> {
        let parsed_args: TodoWriteArgs = serde_json::from_str(&args)?;

        let mut current_todos = self.todos.write().await;

        if parsed_args.merge {
            for new_todo in parsed_args.todos {
                if let Some(existing) = current_todos.iter_mut().find(|t| t.id == new_todo.id) {
                    existing.content = new_todo.content;
                    existing.status = new_todo.status;
                    existing.priority = new_todo.priority;
                } else {
                    current_todos.push(new_todo);
                }
            }
        } else {
            *current_todos = parsed_args.todos;
        }

        let completed_todos: Vec<String> = current_todos
            .iter()
            .filter(|t| t.status == "completed")
            .map(|t| format!("- [x]ID:{}-> {}", t.id, t.content))
            .collect();

        let uncompleted_todos: Vec<String> = current_todos
            .iter()
            .filter(|t| t.status != "completed")
            .map(|t| format!("- [ ]ID:{}-> {}", t.id, t.content))
            .collect();

        // Check if all tasks are completed
        let all_completed =
            !current_todos.is_empty() && current_todos.iter().all(|t| t.status == "completed");
        if all_completed {
            current_todos.clear();
        }

        let mut response = if all_completed {
            "All tasks completed.\n".to_string()
        } else {
            "Update success.\n".to_string()
        };
        response.push_str(&completed_todos.join("\n"));
        response.push_str("\n");
        response.push_str(&uncompleted_todos.join("\n"));

        if all_completed {
            if let Some(summary) = parsed_args.summary {
                response.push_str(&format!("\nSummary: {}\n", summary));
            }
        }
        Ok(response.trim_end().to_string())
    }
}
