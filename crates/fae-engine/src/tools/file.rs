use std::path::PathBuf;

use fae_agent::{Ctx, ToolRequest, ToolResponse, Tools};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use super::{
    DEFAULT_CHANNEL, LIST_DIRECTORY, READ_FILE, WRITE_FILE, effective_tool_name, ok_json,
    parse_arguments, request_tool_name, unsupported_tool,
};

#[derive(Debug, Default)]
pub struct ReadFileTool;

#[derive(Debug, Deserialize)]
struct ReadFileArgs {
    path: PathBuf,
    #[serde(default)]
    with_line_numbers: bool,
    max_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ReadFileResult {
    path: String,
    content: String,
    bytes_read: usize,
    truncated: bool,
}

#[async_trait::async_trait]
impl Tools for ReadFileTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != READ_FILE {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": READ_FILE,
            "description": "Read a UTF-8 text file from disk.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file." },
                    "with_line_numbers": { "type": "boolean", "description": "Prefix each line with a 1-based line number." },
                    "max_bytes": { "type": "integer", "minimum": 1, "description": "Optional maximum number of bytes to return." }
                },
                "required": ["path"]
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != READ_FILE {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: ReadFileArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };

        let bytes = match tokio::fs::read(&args.path).await {
            Ok(bytes) => bytes,
            Err(err) => return Ok(ToolResponse::with_error(500, err.to_string())),
        };

        let max_bytes = args.max_bytes.unwrap_or(bytes.len());
        let truncated = bytes.len() > max_bytes;
        let bytes = &bytes[..bytes.len().min(max_bytes)];
        let mut content = String::from_utf8_lossy(bytes).into_owned();

        if args.with_line_numbers {
            content = content
                .lines()
                .enumerate()
                .map(|(idx, line)| format!("{:>6}\t{line}", idx + 1))
                .collect::<Vec<_>>()
                .join("\n");
        }

        ok_json(ReadFileResult {
            path: args.path.display().to_string(),
            content,
            bytes_read: bytes.len(),
            truncated,
        })
    }
}

#[derive(Debug, Default)]
pub struct WriteFileTool;

#[derive(Debug, Deserialize)]
struct WriteFileArgs {
    path: PathBuf,
    content: String,
    #[serde(default)]
    append: bool,
    #[serde(default)]
    create_parent: bool,
}

#[derive(Debug, Serialize)]
struct WriteFileResult {
    path: String,
    bytes_written: usize,
    append: bool,
}

#[async_trait::async_trait]
impl Tools for WriteFileTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != WRITE_FILE {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": WRITE_FILE,
            "description": "Write UTF-8 text content to a file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to write." },
                    "content": { "type": "string", "description": "File content." },
                    "append": { "type": "boolean", "description": "Append to the file instead of replacing it." },
                    "create_parent": { "type": "boolean", "description": "Create missing parent directories before writing." }
                },
                "required": ["path", "content"]
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != WRITE_FILE {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: WriteFileArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };

        if args.create_parent {
            if let Some(parent) = args.path.parent() {
                if let Err(err) = tokio::fs::create_dir_all(parent).await {
                    return Ok(ToolResponse::with_error(500, err.to_string()));
                }
            }
        }

        let write_result = if args.append {
            async {
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&args.path)
                    .await?;
                file.write_all(args.content.as_bytes()).await
            }
            .await
        } else {
            tokio::fs::write(&args.path, args.content.as_bytes()).await
        };

        if let Err(err) = write_result {
            return Ok(ToolResponse::with_error(500, err.to_string()));
        }

        ok_json(WriteFileResult {
            path: args.path.display().to_string(),
            bytes_written: args.content.len(),
            append: args.append,
        })
    }
}

#[derive(Debug, Default)]
pub struct ListDirectoryTool;

#[derive(Debug, Deserialize)]
struct ListDirectoryArgs {
    path: PathBuf,
}

#[derive(Debug, Serialize)]
struct DirectoryEntry {
    name: String,
    path: String,
    kind: String,
    len: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ListDirectoryResult {
    path: String,
    entries: Vec<DirectoryEntry>,
}

#[async_trait::async_trait]
impl Tools for ListDirectoryTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != LIST_DIRECTORY {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": LIST_DIRECTORY,
            "description": "List files and directories under a path.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path to list." }
                },
                "required": ["path"]
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != LIST_DIRECTORY {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: ListDirectoryArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };

        let mut dir = match tokio::fs::read_dir(&args.path).await {
            Ok(dir) => dir,
            Err(err) => return Ok(ToolResponse::with_error(500, err.to_string())),
        };

        let mut entries = Vec::new();
        loop {
            let entry = match dir.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(err) => return Ok(ToolResponse::with_error(500, err.to_string())),
            };

            let file_type = match entry.file_type().await {
                Ok(file_type) => file_type,
                Err(err) => return Ok(ToolResponse::with_error(500, err.to_string())),
            };
            let metadata = entry.metadata().await.ok();
            let path = entry.path();

            entries.push(DirectoryEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: path.display().to_string(),
                kind: if file_type.is_dir() {
                    "directory"
                } else if file_type.is_file() {
                    "file"
                } else if file_type.is_symlink() {
                    "symlink"
                } else {
                    "other"
                }
                .to_string(),
                len: metadata
                    .filter(|metadata| metadata.is_file())
                    .map(|m| m.len()),
            });
        }

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        ok_json(ListDirectoryResult {
            path: args.path.display().to_string(),
            entries,
        })
    }
}
