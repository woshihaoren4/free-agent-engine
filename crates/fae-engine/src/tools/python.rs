use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::process::Command;

pub struct ExecutePython;

#[async_trait]
impl Tool for ExecutePython {
    fn name(&self) -> &str {
        "execute_python"
    }

    fn description(&self) -> &str {
        "Execute a Python script."
    }

    fn arguments(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "The Python script content to execute."
                }
            },
            "required": ["script"]
        })
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<String> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let script = args_val["script"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("script is required"))?;

        let mut cmd = Command::new("python3");
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(script.as_bytes()).await?;
        }

        let output = child.wait_with_output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() {
            Ok(stdout.to_string())
        } else {
            Err(anyhow::anyhow!(
                "Python script failed with exit code: {:?}\nStdout: {}\nStderr: {}",
                output.status.code(),
                stdout,
                stderr
            ))
        }
    }
}
