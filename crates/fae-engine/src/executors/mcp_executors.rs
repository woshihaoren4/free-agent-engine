use fae_agent::{McpServerConfig, McpToolRequest, McpTools, Select, Task, TaskExecutor, TaskResult, Thing, ThingItem, ThingSelect, ToolRequest, McpToolResult, McpToolResponse};
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

enum McpClient {
    Local {
        child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
        id_counter: u64,
    },
    Remote {
        url: String,
        headers: HashMap<String, String>,
        client: Client,
        id_counter: u64,
    },
}

impl McpClient {
    async fn connect(config: McpServerConfig) -> anyhow::Result<Self> {
        match config {
            McpServerConfig::Local { command, args, env, .. } => {
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
            McpServerConfig::Remote { url, headers, .. } => Ok(Self::Remote {
                url,
                headers,
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
                headers,
            } => {
                let id = *id_counter;
                *id_counter += 1;

                let req = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": params,
                });

                let mut req_builder = client.post(url as &str).json(&req);
                for (k, v) in headers.iter() {
                    req_builder = req_builder.header(k, v);
                }
                req_builder = req_builder.header(reqwest::header::ACCEPT, "application/json, text/event-stream");

                let resp = req_builder.send().await?;
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
            Self::Remote { url, client, headers, .. } => {
                let req = json!({
                    "jsonrpc": "2.0",
                    "method": method,
                    "params": params,
                });
                let mut req_builder = client.post(url as &str).json(&req);
                for (k, v) in headers.iter() {
                    req_builder = req_builder.header(k, v);
                }
                req_builder = req_builder.header(reqwest::header::ACCEPT, "application/json, text/event-stream");
                req_builder.send().await?;
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

    async fn list_tools(&mut self) -> anyhow::Result<Vec<McpToolRequest>> {

        let res:McpTools = self.send_request("tools/list", json!({})).await?;
        Ok(res.tools)
    }

    async fn call_tool(&mut self, name: &str, arguments: Value) -> anyhow::Result<McpToolResult> {
        let params = json!({
            "name": name,
            "arguments": arguments,
        });

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
                    "method": "tools/call",
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
                        return Ok(McpToolResult::with_resp(serde_json::to_string(&res)?));
                    }
                }
            }
            Self::Remote {
                url,
                client,
                id_counter,
                headers,
            } => {
                let id = *id_counter;
                *id_counter += 1;

                let req = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": params,
                });

                let mut req_builder = client.post(url as &str).json(&req);
                for (k, v) in headers.iter() {
                    req_builder = req_builder.header(k, v);
                }
                req_builder = req_builder.header(reqwest::header::ACCEPT, "application/json, text/event-stream");

                let resp = req_builder.send().await?;

                let content_type = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");

                if content_type.contains("text/event-stream") {
                    let (stream_result, chan) = McpToolResult::with_stream();
                    let mut stream = resp.bytes_stream();

                    tokio::spawn(async move {
                        use tokio_stream::StreamExt;
                        let mut buffer = String::new();
                        while let Some(chunk_res) = stream.next().await {
                            if let Ok(bytes) = chunk_res {
                                if let Ok(s) = std::str::from_utf8(bytes.as_ref()) {
                                    buffer.push_str(s);
                                    while let Some(idx) = buffer.find('\n') {
                                        let line = buffer[..idx].to_string();
                                        buffer = buffer[idx + 1..].to_string();

                                        let line = line.trim();
                                        if line.starts_with("data: ") {
                                            let data_str = &line[6..];
                                            if let Ok(mcp_resp) = serde_json::from_str::<McpToolResponse>(data_str) {
                                                let _ = chan.channel.send(mcp_resp);
                                            }
                                        }
                                    }
                                }
                            } else {
                                break;
                            }
                        }
                    });

                    Ok(stream_result)
                } else {
                    let val: Value = resp.json().await?;
                    if let Some(err) = val.get("error") {
                        return Err(anyhow::anyhow!("MCP Error: {}", err));
                    }
                    let res = val.get("result").cloned().unwrap_or(Value::Null);
                    Ok(McpToolResult::with_resp(serde_json::to_string(&res)?))
                }
            }
        }
    }
}

pub struct McpExecutor {
    pub mcp_dir: PathBuf,
    config_cache: tokio::sync::RwLock<HashMap<String, McpServerConfig>>,
}

impl McpExecutor {
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
            drop(cache);
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
impl TaskExecutor for McpExecutor {
    fn desc(&self) -> String {
        "MCP Client Executor".to_string()
    }

    fn channel(&self) -> String {
        "default".to_string()
    }

    async fn execute(&self, mut task: Task) -> anyhow::Result<TaskResult> {
        let req = if let Some(req) = task.into_inner::<ToolRequest>() {
            req
        } else {
            return Err(anyhow::anyhow!("Invalid task input type for MCP"));
        };

        let mut ss = req.tool_name.splitn(2, "__").map(|s| s.to_string()).collect::<Vec<_>>();
        if ss.len() != 2 {
            return Err(anyhow::anyhow!("Invalid mcp tool name format: {}", req.tool_name));
        }
        let mcp_name = ss.remove(0);
        let mcp_tool_name = ss.remove(0);

        let args: Value = serde_json::from_str(&req.arguments)?;

        let config = self.load_config_from_dir(&mcp_name).await?;
        let mut client = McpClient::connect(config).await?;
        client.initialize().await?;

        let result = client
            .call_tool(mcp_tool_name.as_str(), args)
            .await?;

        Ok(TaskResult::success(task.id, task.agent_id).set_data(result))
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

        let mut tools = client.list_tools().await?;

        for i in tools.iter_mut(){
            i.name = format!("{}__{}", mcp_name, i.name);
        }

        let thing = Thing::new(self.channel())
            .add_item(ThingItem::Mcp(tools))
            .into_self();

        Ok(vec![thing])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_remote_mcp_call() -> anyhow::Result<()> {
        let config = McpServerConfig::Remote {
            description: "".to_string(),
            url: "https://mcp.amap.com/mcp?key=8f7be1667d04ac902b87d6ae892733d9".to_string(),
            headers: HashMap::new(),
        };

        let mut client = McpClient::connect(config).await?;
        client.initialize().await?;

        let tools = client.list_tools().await?;
        println!("Tools: {:#?}", tools);

        let args = json!({
            "keywords": "北京大学"
        });

        let result = client.call_tool("maps_text_search", args).await?;
        println!("Call Result: {:#?}", result);

        Ok(())
    }
}
