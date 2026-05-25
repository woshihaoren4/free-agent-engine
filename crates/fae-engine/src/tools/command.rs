use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use tokio::process::Command;

pub struct ExecuteCommand;

#[async_trait]
impl Tool for ExecuteCommand {
    fn name(&self) -> &str {
        "execute_command"
    }

    fn description(&self) -> &str {
        "Execute a shell command."
    }

    fn arguments(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command line string to execute."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional current working directory."
                }
            },
            "required": ["command"]
        }"#
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<String> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let cmd_str = args_val["command"].as_str().ok_or_else(|| anyhow::anyhow!("command is required"))?;
        let cwd = args_val["cwd"].as_str();

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(cmd_str);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(cmd_str);
            c
        };

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let output = cmd.output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        
        if output.status.success() {
            Ok(stdout.to_string())
        } else {
            Err(anyhow::anyhow!("Command failed with exit code: {:?}\nStdout: {}\nStderr: {}", output.status.code(), stdout, stderr))
        }
    }
}
