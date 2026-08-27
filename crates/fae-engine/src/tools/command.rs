use std::path::PathBuf;
use std::process::Stdio;

use fae_agent::{Ctx, ToolRequest, ToolResponse, Tools};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use super::{
    APPLY_PATCH, DEFAULT_CHANNEL, EXECUTE_COMMAND, effective_tool_name, ok_json, parse_arguments,
    request_tool_name, unsupported_tool,
};

#[derive(Debug, Default)]
pub struct ExecuteCommandTool;

#[derive(Debug, Deserialize)]
struct ExecuteCommandArgs {
    command: String,
    cwd: Option<PathBuf>,
    timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
struct CommandResult {
    command: String,
    cwd: Option<String>,
    status_code: Option<i32>,
    success: bool,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

#[async_trait::async_trait]
impl Tools for ExecuteCommandTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != EXECUTE_COMMAND {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": EXECUTE_COMMAND,
            "description": "Execute a shell command and return stdout, stderr, and exit status.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute." },
                    "cwd": { "type": "string", "description": "Optional working directory." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "description": "Optional command timeout in seconds. Defaults to 60." }
                },
                "required": ["command"]
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != EXECUTE_COMMAND {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: ExecuteCommandArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };

        run_shell_command(args.command, args.cwd, args.timeout_secs).await
    }
}

#[derive(Debug, Default)]
pub struct ApplyPatchTool;

#[derive(Debug, Deserialize)]
struct ApplyPatchArgs {
    patch: String,
    cwd: Option<PathBuf>,
    strip: Option<u8>,
    timeout_secs: Option<u64>,
}

#[async_trait::async_trait]
impl Tools for ApplyPatchTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != APPLY_PATCH {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": APPLY_PATCH,
            "description": "Apply a unified diff patch using the system patch command.",
            "parameters": {
                "type": "object",
                "properties": {
                    "patch": { "type": "string", "description": "Unified diff content." },
                    "cwd": { "type": "string", "description": "Optional working directory." },
                    "strip": { "type": "integer", "minimum": 0, "description": "Path components to strip. Defaults to 1." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "description": "Optional timeout in seconds. Defaults to 60." }
                },
                "required": ["patch"]
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != APPLY_PATCH {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: ApplyPatchArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };

        let strip = args.strip.unwrap_or(1);
        let mut command = Command::new("patch");
        command
            .arg("--forward")
            .arg("--batch")
            .arg(format!("-p{strip}"))
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(cwd) = &args.cwd {
            command.current_dir(cwd);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => return Ok(ToolResponse::with_error(500, err.to_string())),
        };

        if let Some(mut stdin) = child.stdin.take() {
            if let Err(err) = stdin.write_all(args.patch.as_bytes()).await {
                return Ok(ToolResponse::with_error(500, err.to_string()));
            }
        }

        let timeout_secs = args.timeout_secs.unwrap_or(60);
        let output =
            match timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(err)) => return Ok(ToolResponse::with_error(500, err.to_string())),
                Err(_) => {
                    return ok_json(CommandResult {
                        command: "patch".to_string(),
                        cwd: args.cwd.map(|cwd| cwd.display().to_string()),
                        status_code: None,
                        success: false,
                        stdout: String::new(),
                        stderr: format!("command timed out after {timeout_secs}s"),
                        timed_out: true,
                    });
                }
            };

        ok_json(CommandResult {
            command: "patch".to_string(),
            cwd: args.cwd.map(|cwd| cwd.display().to_string()),
            status_code: output.status.code(),
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            timed_out: false,
        })
    }
}

async fn run_shell_command(
    command: String,
    cwd: Option<PathBuf>,
    timeout_secs: Option<u64>,
) -> anyhow::Result<ToolResponse> {
    #[cfg(target_os = "windows")]
    let mut child = {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(&command);
        cmd
    };

    #[cfg(not(target_os = "windows"))]
    let mut child = {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&command);
        cmd
    };

    child.stdout(Stdio::piped()).stderr(Stdio::piped());
    child.kill_on_drop(true);
    if let Some(cwd) = &cwd {
        child.current_dir(cwd);
    }

    let timeout_secs = timeout_secs.unwrap_or(60);
    let output = match timeout(Duration::from_secs(timeout_secs), child.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Ok(ToolResponse::with_error(500, err.to_string())),
        Err(_) => {
            return ok_json(CommandResult {
                command,
                cwd: cwd.map(|cwd| cwd.display().to_string()),
                status_code: None,
                success: false,
                stdout: String::new(),
                stderr: format!("command timed out after {timeout_secs}s"),
                timed_out: true,
            });
        }
    };

    ok_json(CommandResult {
        command,
        cwd: cwd.map(|cwd| cwd.display().to_string()),
        status_code: output.status.code(),
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        timed_out: false,
    })
}
