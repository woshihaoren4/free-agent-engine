mod command;
mod file;
mod http;
mod python;

use crate::ToolsRuntime;
use fae_agent::{Ctx, ToolRequest, ToolResponse, Tools};
use serde_json::{Value, json};

pub use command::{ApplyPatchTool, ExecuteCommandTool};
pub use file::{ListDirectoryTool, ReadFileTool, WriteFileTool};
pub use http::SendHttpRequestTool;
pub use python::ExecutePythonTool;

pub const DEFAULT_CHANNEL: &str = "default";
pub const EXECUTE_COMMAND: &str = "execute_command";
pub const READ_FILE: &str = "read_file";
pub const WRITE_FILE: &str = "write_file";
pub const LIST_DIRECTORY: &str = "list_directory";
pub const APPLY_PATCH: &str = "apply_patch";
pub const SEND_HTTP_REQUEST: &str = "send_http_request";
pub const EXECUTE_PYTHON: &str = "execute_python";

pub fn register_default_tools(runtime: &mut ToolsRuntime) {
    runtime.add_tool(Box::new(DefaultTools::default()));
}

#[derive(Debug, Default)]
pub struct DefaultTools {
    execute_command: ExecuteCommandTool,
    read_file: ReadFileTool,
    write_file: WriteFileTool,
    list_directory: ListDirectoryTool,
    apply_patch: ApplyPatchTool,
    send_http_request: SendHttpRequestTool,
    execute_python: ExecutePythonTool,
}

#[async_trait::async_trait]
impl Tools for DefaultTools {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        match effective_tool_name(tool_name) {
            DEFAULT_CHANNEL => Ok(json!({
                "channel": DEFAULT_CHANNEL,
                "tools": [
                    EXECUTE_COMMAND,
                    READ_FILE,
                    WRITE_FILE,
                    LIST_DIRECTORY,
                    APPLY_PATCH,
                    SEND_HTTP_REQUEST,
                    EXECUTE_PYTHON
                ]
            })),
            EXECUTE_COMMAND => self.execute_command.desc(ctx, tool_name).await,
            READ_FILE => self.read_file.desc(ctx, tool_name).await,
            WRITE_FILE => self.write_file.desc(ctx, tool_name).await,
            LIST_DIRECTORY => self.list_directory.desc(ctx, tool_name).await,
            APPLY_PATCH => self.apply_patch.desc(ctx, tool_name).await,
            SEND_HTTP_REQUEST => self.send_http_request.desc(ctx, tool_name).await,
            EXECUTE_PYTHON => self.execute_python.desc(ctx, tool_name).await,
            _ => Err(unsupported_tool(tool_name)),
        }
    }

    async fn exec(&self, ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        let tool_name = request_tool_name(&req).to_string();

        match tool_name.as_str() {
            EXECUTE_COMMAND => self.execute_command.exec(ctx, req).await,
            READ_FILE => self.read_file.exec(ctx, req).await,
            WRITE_FILE => self.write_file.exec(ctx, req).await,
            LIST_DIRECTORY => self.list_directory.exec(ctx, req).await,
            APPLY_PATCH => self.apply_patch.exec(ctx, req).await,
            SEND_HTTP_REQUEST => self.send_http_request.exec(ctx, req).await,
            EXECUTE_PYTHON => self.execute_python.exec(ctx, req).await,
            _ => Err(unsupported_tool(req.get_tool_name())),
        }
    }
}

fn parse_arguments<T>(arguments: &str) -> Result<T, fae_agent::ToolResponse>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_str(arguments).map_err(|err| {
        fae_agent::ToolResponse::with_error(400, format!("invalid arguments: {err}"))
    })
}

fn ok_json(value: impl serde::Serialize) -> anyhow::Result<fae_agent::ToolResponse> {
    Ok(fae_agent::ToolResponse::with_result(serde_json::to_string(
        &value,
    )?))
}

fn effective_tool_name(name: &str) -> &str {
    name.split_once("__").map(|(_, name)| name).unwrap_or(name)
}

fn request_tool_name(req: &fae_agent::ToolRequest) -> &str {
    effective_tool_name(req.get_tool_name())
}

fn unsupported_tool(tool_name: &str) -> anyhow::Error {
    anyhow::anyhow!("unsupported tool `{tool_name}`")
}
