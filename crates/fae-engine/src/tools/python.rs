use std::path::PathBuf;
use std::process::Stdio;

use fae_agent::{Ctx, ToolRequest, ToolResponse, Tools};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use super::{
    DEFAULT_CHANNEL, EXECUTE_PYTHON, effective_tool_name, ok_json, parse_arguments,
    request_tool_name, unsupported_tool,
};

#[derive(Debug, Default)]
pub struct ExecutePythonTool;

#[derive(Debug, Deserialize)]
struct ExecutePythonArgs {
    script: String,
    cwd: Option<PathBuf>,
    python: Option<String>,
    timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ExecutePythonResult {
    python: String,
    cwd: Option<String>,
    status_code: Option<i32>,
    success: bool,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

#[async_trait::async_trait]
impl Tools for ExecutePythonTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != EXECUTE_PYTHON {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": EXECUTE_PYTHON,
            "description": "Execute Python code with python3 by default.",
            "parameters": {
                "type": "object",
                "properties": {
                    "script": { "type": "string", "description": "Python source code to execute." },
                    "cwd": { "type": "string", "description": "Optional working directory." },
                    "python": { "type": "string", "description": "Python executable. Defaults to python3." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "description": "Optional timeout in seconds. Defaults to 60." }
                },
                "required": ["script"]
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != EXECUTE_PYTHON {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: ExecutePythonArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };

        let python = args.python.unwrap_or_else(|| "python3".to_string());
        let mut command = Command::new(&python);
        command
            .arg("-c")
            .arg(&args.script)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(cwd) = &args.cwd {
            command.current_dir(cwd);
        }

        let timeout_secs = args.timeout_secs.unwrap_or(60);
        let output = match timeout(Duration::from_secs(timeout_secs), command.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => return Ok(ToolResponse::with_error(500, err.to_string())),
            Err(_) => {
                return ok_json(ExecutePythonResult {
                    python,
                    cwd: args.cwd.map(|cwd| cwd.display().to_string()),
                    status_code: None,
                    success: false,
                    stdout: String::new(),
                    stderr: format!("python execution timed out after {timeout_secs}s"),
                    timed_out: true,
                });
            }
        };

        ok_json(ExecutePythonResult {
            python,
            cwd: args.cwd.map(|cwd| cwd.display().to_string()),
            status_code: output.status.code(),
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            timed_out: false,
        })
    }
}
