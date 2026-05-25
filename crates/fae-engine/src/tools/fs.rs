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

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file."
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
        let path = args_val["path"].as_str().ok_or_else(|| anyhow::anyhow!("path is required"))?;
        let content = args_val["content"].as_str().ok_or_else(|| anyhow::anyhow!("content is required"))?;
        fs::write(path, content).await?;
        Ok(format!("Successfully wrote to {}", path))
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
