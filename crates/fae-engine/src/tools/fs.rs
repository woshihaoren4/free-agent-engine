use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use fae_agent::{GLOBAL_KEY_AGENT_ID, GLOBAL_KEY_PROJECT_DIR, GLOBAL_KEY_WORKSPACE, ToolResponse};
use serde_json::Value;
use std::path::PathBuf;
use tokio::fs;

#[derive(Debug)]
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file."
    }

    fn arguments(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the file to read."
                },
                "with_line_numbers": {
                    "type": "boolean",
                    "description": "Add line numbers。When reading structured content such as code and configuration, try to set it to true.",
                    "default": false
                }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let path = args_val["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path is required"))?;
        let with_line_numbers = args_val
            .get("with_line_numbers")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let content = fs::read_to_string(path).await?;
        let content = if with_line_numbers {
            add_line_numbers(&content)
        } else {
            content
        };
        Ok(ToolResponse::with_result(content))
    }
}

fn add_line_numbers(content: &str) -> String {
    let line_count = content.lines().count();
    if line_count == 0 {
        return String::new();
    }

    let width = line_count.to_string().len();
    let mut result = content
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{:>width$}: {}", index + 1, line, width = width))
        .collect::<Vec<_>>()
        .join("\n");

    if content.ends_with('\n') {
        result.push('\n');
    }

    result
}

#[derive(Default, Debug)]
pub struct WriteFile;

impl WriteFile {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Can only write files in the allowed directories."
    }

    fn arguments(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the file to write."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write into the file."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(&self, iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let path_str = args_val["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path is required"))?;
        let content = args_val["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("content is required"))?;

        let fae_home_dir = fae_agent::fae_home();

        let mut allowed_dirs = vec![
            fae_home_dir.join("skills"),
            fae_home_dir.join("prompt"),
            fae_home_dir.join("mcp"),
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        ];

        if let Some(ws) = iden.get(GLOBAL_KEY_WORKSPACE) {
            if let Some(aid) = iden.get(GLOBAL_KEY_AGENT_ID) {
                allowed_dirs.push(fae_home_dir.join(ws));
            }
        }
        if let Some(pdir) = iden.get(GLOBAL_KEY_PROJECT_DIR) {
            allowed_dirs.push(PathBuf::from(pdir));
        }

        let allowed_dirs_canonical: Vec<std::path::PathBuf> = allowed_dirs
            .into_iter()
            .map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir))
            .collect();

        let target_path = std::path::Path::new(path_str);
        let final_path = if target_path.is_absolute() {
            target_path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(target_path)
        };

        let parent = final_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""));
        let parent_canonical = std::fs::canonicalize(parent).map_err(|e| {
            anyhow::anyhow!("Failed to canonicalize parent directory {:?}: {}. Does the parent directory exist?", parent, e)
        })?;

        let mut is_allowed = false;
        for allowed_dir_canonical in &allowed_dirs_canonical {
            if parent_canonical.starts_with(allowed_dir_canonical) {
                is_allowed = true;
                break;
            }
        }

        if !is_allowed {
            return Err(anyhow::anyhow!(
                "Permission denied: cannot write outside of allowed directories {:?}",
                allowed_dirs_canonical
            ));
        }

        fs::write(&final_path, content).await?;
        Ok(ToolResponse::with_result(format!(
            "Successfully wrote to {}",
            final_path.display()
        )))
    }
}

#[derive(Debug)]
pub struct ListDirectory;

#[async_trait]
impl Tool for ListDirectory {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List the contents of a directory."
    }

    fn arguments(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the directory."
                }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let path = args_val["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path is required"))?;

        let mut entries = fs::read_dir(path).await?;
        let mut result = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            result.push(entry.file_name().to_string_lossy().to_string());
        }

        Ok(ToolResponse::with_result(serde_json::to_string(&result)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_file_arguments_include_optional_line_numbers() {
        let arguments = ReadFile.arguments();

        assert_eq!(
            arguments["properties"]["with_line_numbers"]["default"],
            json!(false)
        );
        assert_eq!(
            arguments["properties"]["with_line_numbers"]["type"],
            json!("boolean")
        );
        assert!(
            !arguments["required"]
                .as_array()
                .expect("required should be an array")
                .contains(&json!("with_line_numbers"))
        );
    }

    #[test]
    fn add_line_numbers_preserves_trailing_newline() {
        let content = "alpha\n\nomega\n";

        assert_eq!(add_line_numbers(content), "1: alpha\n2: \n3: omega\n");
    }
}
