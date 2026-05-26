use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use tokio::fs;

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file."
    }

    fn arguments(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the file to read."
                }
            },
            "required": ["path"]
        }"#
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<String> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let path = args_val["path"].as_str().ok_or_else(|| anyhow::anyhow!("path is required"))?;
        let content = fs::read_to_string(path).await?;
        Ok(content)
    }
}

#[derive(Default)]
pub struct WriteFile {
    pub allowed_dir: Option<String>,
}

impl WriteFile {
    pub fn new(allowed_dir: Option<String>) -> Self {
        Self { allowed_dir }
    }

    fn get_allowed_dir(&self) -> std::path::PathBuf {
        if let Some(dir) = &self.allowed_dir {
            std::path::PathBuf::from(dir)
        } else if let Ok(dir) = std::env::var("FAE_TOOL_FS_WRITE_DIR") {
            std::path::PathBuf::from(dir)
        } else {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        }
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Can only write files in the allowed directory."
    }

    fn arguments(&self) -> &str {
        r#"{
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
        }"#
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<String> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let path_str = args_val["path"].as_str().ok_or_else(|| anyhow::anyhow!("path is required"))?;
        let content = args_val["content"].as_str().ok_or_else(|| anyhow::anyhow!("content is required"))?;
        
        let allowed_dir = self.get_allowed_dir();
        let allowed_dir_canonical = std::fs::canonicalize(&allowed_dir)
            .unwrap_or_else(|_| allowed_dir.clone());
            
        let target_path = std::path::Path::new(path_str);
        let final_path = if target_path.is_absolute() {
            target_path.to_path_buf()
        } else {
            allowed_dir.join(target_path)
        };
        
        let parent = final_path.parent().unwrap_or_else(|| std::path::Path::new(""));
        let parent_canonical = std::fs::canonicalize(parent).map_err(|e| {
            anyhow::anyhow!("Failed to canonicalize parent directory {:?}: {}. Does the parent directory exist?", parent, e)
        })?;
        
        if !parent_canonical.starts_with(&allowed_dir_canonical) {
            return Err(anyhow::anyhow!("Permission denied: cannot write outside of allowed directory {:?}", allowed_dir));
        }
        
        fs::write(&final_path, content).await?;
        Ok(format!("Successfully wrote to {}", final_path.display()))
    }
}

pub struct ListDirectory;

#[async_trait]
impl Tool for ListDirectory {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List the contents of a directory."
    }

    fn arguments(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the directory."
                }
            },
            "required": ["path"]
        }"#
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<String> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let path = args_val["path"].as_str().ok_or_else(|| anyhow::anyhow!("path is required"))?;
        
        let mut entries = fs::read_dir(path).await?;
        let mut result = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            result.push(entry.file_name().to_string_lossy().to_string());
        }
        
        Ok(serde_json::to_string(&result)?)
    }
}
