use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use fae_agent::ToolResponse;

#[derive(Debug)]
pub struct ExecuteCommand {
    // 待确认的命令：key为确认码，value为截至时间戳utc时间second
    pending_confirmations: Mutex<HashMap<String, u64>>,
    blacklist: Vec<String>,
}

impl Default for ExecuteCommand {
    fn default() -> Self {
        Self {
            pending_confirmations: Mutex::new(HashMap::new()),
            blacklist: vec![
                "rm".to_string(),
                "kill".to_string(),
                "del".to_string(),
                "mv".to_string(),
                "fdisk".to_string(),
                "mkfs".to_string(),
                "dd".to_string(),
                "format".to_string(),
                "shutdown".to_string(),
                "reboot".to_string(),
                "halt".to_string(),
                "poweroff".to_string(),
            ],
        }
    }
}

impl ExecuteCommand {
    pub fn set_blacklist(mut self, blacklist: Vec<String>) -> Self {
        self.blacklist = blacklist;
        self
    }

    pub fn get_blacklist(&self) -> &Vec<String> {
        &self.blacklist
    }

    pub fn append_blacklist(mut self, blacklist: Vec<String>) -> Self {
        self.blacklist.extend(blacklist);
        self
    }

    fn generate_confirm_code(&self) -> String {
        let iden_key = format!("{}", wd_tools::uuid::v4());
        // 3分钟过期时间
        let expire_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60 * 3;
        let mut pending = self.pending_confirmations.lock().unwrap();
        pending.insert(iden_key.clone(), expire_time);
        iden_key
    }

    fn verify_and_remove_confirm_code(&self, code: &str) -> Result<bool, String> {
        let mut pending = self.pending_confirmations.lock().unwrap();
        if let Some(expire_time) = pending.remove(code) {
            if expire_time
                > std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            {
                Ok(true)
            } else {
                Err("Invalid confirm_code. Command execution denied.".to_string())
            }
        } else {
            Ok(false)
        }
    }
}

#[async_trait]
impl Tool for ExecuteCommand {
    fn name(&self) -> &str {
        "execute_command"
    }

    fn description(&self) -> &str {
        "Execute a shell command."
    }

    fn arguments(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command line string to execute."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional current working directory."
                },
                "confirm_code": {
                    "type": "string",
                    "description": "The confirmation code required for dangerous commands. Leave empty unless prompted. default is empty."
                }
            },
            "required": ["command"]
        })
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let cmd_str = args_val["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("command is required"))?;
        let cwd = args_val["cwd"].as_str();
        let confirm_code = args_val["confirm_code"].as_str();

        let is_dangerous = {
            let cmd = cmd_str.trim();
            let first_word = cmd.split_whitespace().next().unwrap_or("");
            self.blacklist.iter().any(|b| b == first_word)
        };

        if is_dangerous {
            let mut require_confirm = false;

            if let Some(code) = confirm_code {
                match self.verify_and_remove_confirm_code(code) {
                    Ok(true) => {}                       // Valid, proceed
                    Ok(false) => require_confirm = true, // Code not found, require new confirmation
                    Err(e) => return Err(anyhow::anyhow!(e)),
                }
            } else {
                require_confirm = true;
            }

            if require_confirm {
                let generated_code = self.generate_confirm_code();
                return Ok(ToolResponse::with_result(format!(
                    "Warning: You are trying to execute a dangerous command. Immediately terminate the session and ask the user for manual confirmation. After the user confirms a second time, set `confirm_code` is `{}`",
                    generated_code
                )));
            }
        }

        let resp = ToolResponse::with_stream_chan();

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

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn()?;

        let stdout = child.stdout.take().expect("Failed to capture stdout");
        let stderr = child.stderr.take().expect("Failed to capture stderr");

        let chan = resp.clone();
        tokio::spawn(async move {
            let sub_chan = chan.clone();
            let stdout_task = tokio::spawn(async move {
                let mut stdout_output = String::new();
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(mut line)) = reader.next_line().await {
                    line.push_str("\n");
                    stdout_output.push_str(&line);
                    if let Err(e) = sub_chan.streaming_push(line).await {
                        wd_log::log_error_ln!("Failed to send stdout line: {:?}", e);
                    }
                }
                stdout_output
            });

            let sub_chan = chan.clone();
            let stderr_task = tokio::spawn(async move {
                let mut stderr_output = String::new();
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(mut line)) = reader.next_line().await {
                    line.push_str("\n");
                    stderr_output.push_str(&line);
                    if let Err(e) = sub_chan.streaming_push(line).await {
                        wd_log::log_error_ln!("Failed to send stderr line: {:?}", e);
                    }
                }
                stderr_output
            });

            let stdout_output = stdout_task.await.unwrap_or_default();
            let stderr_output = stderr_task.await.unwrap_or_default();

            let status = match child.wait().await{
                Ok(status) => status,
                Err(e)=>{
                    let err = format!("Command Over error : {:?}\nStdout: {}\nStderr: {}",
                                      e.to_string(),
                                      stdout_output,
                                      stderr_output);
                    if let Err(e) = chan.error_completed_push(err).await {
                        wd_log::log_error_ln!("Failed to send stderr output: {:?}", e);
                    }
                    return ;
                }
            };

            if status.success() {
                if let Err(e) = chan.success_completed_push(stdout_output).await {
                    wd_log::log_error_ln!("Failed to send stdout output: {:?}", e);
                }
            } else {
                let err = format!("Command failed with exit code: {:?}\nStdout: {}\nStderr: {}",
                status.code(),
                stdout_output,
                stderr_output);
                if let Err(e) = chan.error_completed_push(err).await {
                    wd_log::log_error_ln!("Failed to send stderr output: {:?}", e);
                }
            }
        });
        Ok(resp)
    }
}
