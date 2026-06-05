use fae_agent::{
    Select, Task, TaskExecutor, TaskResult, Thing, ThingItem, ThingSelect, ToolRequest,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Debug, Deserialize)]
pub struct McpConfigFile {
    #[serde(rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum McpServerConfig {
    Local {
        command: String,
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Remote {
        url: String,
    },
}

#[derive(Deserialize)]
struct McpToolInfo {
    name: String,
    description: Option<String>,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

enum McpClient {
    Local {
        child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
        id_counter: u64,
    },
    Remote {
        url: String,
        client: Client,
        id_counter: u64,
    },
}

impl McpClient {
    async fn connect(config: McpServerConfig) -> anyhow::Result<Self> {
        match config {
            McpServerConfig::Local { command, args, env } => {
                let mut cmd = Command::new(&command);
                cmd.args(&args);
                cmd.envs(&env);
                cmd.stdin(Stdio::piped());
                cmd.stdout(Stdio::piped());
                cmd.stderr(Stdio::inherit());

                let mut child = cmd.spawn()?;
                let stdin = child.stdin.take().unwrap();
                let stdout = BufReader::new(child.stdout.take().unwrap());

                Ok(Self::Local {
                    child,
                    stdin,
                    stdout,
                    id_counter: 1,
                })
            }
            McpServerConfig::Remote { url } => Ok(Self::Remote {
                url,
                client: Client::new(),
                id_counter: 1,
            }),
        }
    }

    async fn send_request<T: Serialize, R: serde::de::DeserializeOwned>(
        &mut self,
        method: &str,
        params: T,
    ) -> anyhow::Result<R> {
        match self {
            Self::Local {
                stdin,
                stdout,
                id_counter,
                ..
            } => {
                let id = *id_counter;
                *id_counter += 1;

                let req = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": params,
                });

                let mut req_str = serde_json::to_string(&req)?;
                req_str.push('\n');
                stdin.write_all(req_str.as_bytes()).await?;
                stdin.flush().await?;

                loop {
                    let mut line = String::new();
                    let n = stdout.read_line(&mut line).await?;
                    if n == 0 {
                        return Err(anyhow::anyhow!("MCP server closed stdout"));
                    }

                    let val: Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => continue, // Ignore non-JSON lines
                    };

                    if val.get("id").and_then(|i| i.as_u64()) == Some(id) {
                        if let Some(err) = val.get("error") {
                            return Err(anyhow::anyhow!("MCP Error: {}", err));
                        }
                        let res = val.get("result").cloned().unwrap_or(Value::Null);
                        return Ok(serde_json::from_value(res)?);
                    }
                }
            }
            Self::Remote {
                url,
                client,
                id_counter,
            } => {
                let id = *id_counter;
                *id_counter += 1;

                let req = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": params,
                });

                let resp = client.post(url as &str).json(&req).send().await?;
                let val: Value = resp.json().await?;

                if let Some(err) = val.get("error") {
                    return Err(anyhow::anyhow!("MCP Error: {}", err));
                }
                let res = val.get("result").cloned().unwrap_or(Value::Null);
                Ok(serde_json::from_value(res)?)
            }
        }
    }

    async fn send_notification<T: Serialize>(
        &mut self,
        method: &str,
        params: T,
    ) -> anyhow::Result<()> {
        match self {
            Self::Local { stdin, .. } => {
                let req = json!({
                    "jsonrpc": "2.0",
                    "method": method,
                    "params": params,
                });
                let mut req_str = serde_json::to_string(&req)?;
                req_str.push('\n');
                stdin.write_all(req_str.as_bytes()).await?;
                stdin.flush().await?;
            }
            Self::Remote { url, client, .. } => {
                let req = json!({
                    "jsonrpc": "2.0",
                    "method": method,
                    "params": params,
                });
                client.post(url as &str).json(&req).send().await?;
            }
        }
        Ok(())
    }

    async fn initialize(&mut self) -> anyhow::Result<()> {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "fae-engine",
                "version": "0.1.0"
            }
        });
        let _res: Value = self.send_request("initialize", params).await?;
        self.send_notification("notifications/initialized", json!({}))
            .await?;
        Ok(())
    }

    async fn list_tools(&mut self) -> anyhow::Result<Vec<McpToolInfo>> {
        #[derive(Deserialize)]
        struct ToolsResult {
            tools: Vec<McpToolInfo>,
        }
        let res: ToolsResult = self.send_request("tools/list", json!({})).await?;
        Ok(res.tools)
    }

    async fn call_tool(&mut self, name: &str, arguments: Value) -> anyhow::Result<Value> {
        let params = json!({
            "name": name,
            "arguments": arguments,
        });
        let res: Value = self.send_request("tools/call", params).await?;
        Ok(res)
    }
}

pub struct McpClientExecutor {
    pub mcp_dir: PathBuf,
    config_cache: tokio::sync::RwLock<HashMap<String, McpServerConfig>>,
}

impl McpClientExecutor {
    pub fn new(mcp_dir: impl Into<PathBuf>) -> Self {
        Self {
            mcp_dir: mcp_dir.into(),
            config_cache: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    async fn load_config_from_dir(&self, mcp_name: &str) -> anyhow::Result<McpServerConfig> {
        {
            let cache = self.config_cache.read().await;
            if let Some(config) = cache.get(mcp_name) {
                return Ok(config.clone());
            }
        }

        let mut cache = self.config_cache.write().await;
        if !self.mcp_dir.exists() {
            return Err(anyhow::anyhow!("MCP dir not found: {:?}", self.mcp_dir));
        }

        let mut entries = tokio::fs::read_dir(&self.mcp_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
                let content = tokio::fs::read_to_string(&path).await?;
                if let Ok(config_file) = serde_json::from_str::<McpConfigFile>(&content) {
                    for (name, server) in config_file.mcp_servers {
                        cache.insert(name, server);
                    }
                }
            }
        }

        if let Some(config) = cache.get(mcp_name) {
            Ok(config.clone())
        } else {
            Err(anyhow::anyhow!("MCP Server config '{}' not found", mcp_name))
        }
    }
}

#[async_trait::async_trait]
impl TaskExecutor for McpClientExecutor {
    fn desc(&self) -> String {
        "MCP Client Executor".to_string()
    }

    fn channel(&self) -> String {
        "mcp".to_string()
    }

    async fn execute(&self, mut task: Task) -> anyhow::Result<TaskResult> {
        let req = if let Some(req) = task.into_inner::<ToolRequest>() {
            req
        } else {
            return Err(anyhow::anyhow!("Invalid task input type for MCP"));
        };

        let mcp_name = req.tool_name;

        #[derive(Deserialize)]
        struct McpCallArgs {
            function_name: String,
            arguments: Value,
        }
        let args: McpCallArgs = serde_json::from_str(&req.arguments)?;

        let config = self.load_config_from_dir(&mcp_name).await?;
        let mut client = McpClient::connect(config).await?;
        client.initialize().await?;

        let result = client
            .call_tool(&args.function_name, args.arguments)
            .await?;

        let content_str = serde_json::to_string(&result)?;

        Ok(TaskResult::success(task.id, task.agent_id).set_data(content_str))
    }

    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>> {
        let (channel, mcp_name) = if let ThingSelect::Mcp(channel, name) = select.select {
            (channel, name)
        } else {
            return Err(fae_agent::Error::NoSupport.into());
        };

        if channel != self.channel() {
            return Err(fae_agent::Error::NoSupport.into());
        }

        let config = self.load_config_from_dir(&mcp_name).await?;
        let mut client = McpClient::connect(config).await?;
        client.initialize().await?;

        let tools = client.list_tools().await?;

        let mut desc = format!("MCP Server: {}\nAvailable functions:\n", mcp_name);
        let mut function_names = vec![];
        for t in &tools {
            desc.push_str(&format!(
                "- {}: {}\n  Schema: {}\n",
                t.name,
                t.description.as_deref().unwrap_or(""),
                serde_json::to_string(&t.input_schema).unwrap_or_default()
            ));
            function_names.push(t.name.clone());
        }

        let args_schema = json!({
            "type": "object",
            "properties": {
                "function_name": {
                    "type": "string",
                    "enum": function_names,
                    "description": "The name of the function to call."
                },
                "arguments": {
                    "type": "object",
                    "description": "The arguments for the function. Please refer to the specific function schema."
                }
            },
            "required": ["function_name", "arguments"]
        });

        let thing = Thing::new(self.channel())
            .add_item(ThingItem::Tool(desc, args_schema))
            .into_self();

        Ok(vec![thing])
    }
}
