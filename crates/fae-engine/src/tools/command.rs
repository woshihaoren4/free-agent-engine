use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use tokio::process::Command;
use std::sync::Mutex;
use std::collections::HashMap;

pub struct ExecuteCommand {
    pending_confirmations: Mutex<HashMap<String, String>>,
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

    fn generate_confirm_code(&self, iden_key: String) -> String {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let generated_code = format!("{:016x}", nanos);
        let mut pending = self.pending_confirmations.lock().unwrap();
        pending.insert(iden_key, generated_code.clone());
        generated_code
    }

    fn verify_and_remove_confirm_code(&self, iden_key: &str, code: &str) -> Result<bool, String> {
        let mut pending = self.pending_confirmations.lock().unwrap();
        if let Some(expected_code) = pending.get(iden_key) {
            if code == expected_code {
                pending.remove(iden_key);
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
                },
                "confirm_code": {
                    "type": "string",
                    "description": "The confirmation code required for dangerous commands. Leave empty unless prompted. default is empty."
                }
            },
            "required": ["command"]
        }"#
    }

    async fn call(&self, iden: IdenInfo, args: String) -> anyhow::Result<String> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let cmd_str = args_val["command"].as_str().ok_or_else(|| anyhow::anyhow!("command is required"))?;
        let cwd = args_val["cwd"].as_str();
        let confirm_code = args_val["confirm_code"].as_str();

        let is_dangerous = {
            let cmd = cmd_str.trim();
            let first_word = cmd.split_whitespace().next().unwrap_or("");
            self.blacklist.iter().any(|b| b == first_word)
        };

        if is_dangerous {
            let iden_key = format!("{}:{}", iden.get_agent_id(),wd_tools::uuid::v4());
            let mut require_confirm = false;
            
            if let Some(code) = confirm_code {
                match self.verify_and_remove_confirm_code(&iden_key, code) {
                    Ok(true) => {} // Valid, proceed
                    Ok(false) => require_confirm = true, // Code not found, require new confirmation
                    Err(e) => return Err(anyhow::anyhow!(e)),
                }
            } else {
                require_confirm = true;
            }

            if require_confirm {
                let generated_code = self.generate_confirm_code(iden_key);
                return Ok(format!("Warning: You are trying to execute a dangerous command. please user to confirm, please call this tool again with the parameter `confirm_code` set to `{}`", generated_code));
            }
        }

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
