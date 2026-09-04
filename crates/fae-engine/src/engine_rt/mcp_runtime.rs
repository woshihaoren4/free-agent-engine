use std::{collections::HashMap, path::{Path, PathBuf}, process::Stdio};

use fae_agent::{
    Event, EventType, McpQuery, McpRequest, McpResponse, McpServerConfig, McpToolInfo,
    RuntimeSelectExec, TaskError, TaskReq, TaskResp, TaskType,
};
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, Tool},
    transport::{
        TokioChildProcess, StreamableHttpClientTransport,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use wd_tools::channel::{Channel, Receiver, Sender};

use super::default_fae_host;

#[derive(Debug, Deserialize)]
struct McpConfigFile {
    #[serde(rename = "mcpServers")]
    mcp_servers: HashMap<String, McpServerConfig>,
}

#[derive(Debug)]
pub struct McpRuntime {
    mcp_dir: PathBuf,
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Default for McpRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl McpRuntime {
    pub const ID: &'static str = "mcp_default";

    pub fn new() -> Self {
        Self::with_mcp_dir(default_fae_host().join("mcp"))
    }

    pub fn with_mcp_dir(mcp_dir: impl Into<PathBuf>) -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            mcp_dir: mcp_dir.into(),
            event_sender,
            event_receiver,
        }
    }

    pub fn mcp_dir(&self) -> &Path {
        &self.mcp_dir
    }

    async fn load_server_config(&self, server: &str) -> fae_agent::Result<McpServerConfig> {
        Ok(load_server_config(&self.mcp_dir, server).await?)
    }

    pub async fn query(&self, server: &str) -> fae_agent::Result<Vec<McpToolInfo>> {
        let config = self.load_server_config(server).await?;
        Ok(list_tools(server, config).await?)
    }

    async fn execute(&self, request: McpRequest) -> fae_agent::Result<McpResponse> {
        let config = self.load_server_config(&request.server).await?;
        Ok(call_tool(config, request).await?)
    }
}

#[async_trait::async_trait]
impl RuntimeSelectExec<McpRequest, McpResponse, McpQuery, Vec<McpToolInfo>> for McpRuntime {
    fn id(&self) -> &str {
        Self::ID
    }

    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Mcp]
    }

    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.event_receiver.clone())
    }

    async fn select(
        &self,
        ty: TaskType,
        query: McpQuery,
    ) -> fae_agent::Result<Vec<McpToolInfo>> {
        if ty != TaskType::Mcp {
            return Err(fae_agent::Error::RuntimeNoSupport);
        }
        self.query(&query.server).await
    }

    async fn spawn(&self, task: TaskReq<McpRequest>) -> fae_agent::Result<()> {
        let TaskReq { ctx, mut meta, req } = task;
        let config = self.load_server_config(&req.server).await?;
        let event_sender = self.event_sender.clone();

        tokio::spawn(async move {
            if meta.publisher.is_empty() {
                meta.publisher = Self::ID.to_string();
            }
            let response_ctx = ctx.clone();
            let error_meta = meta.clone();
            let result = call_tool(config, req).await.map(|resp| Event {
                from_rt_id: Self::ID.to_string(),
                event_type: EventType::TaskResult(
                    TaskResp {
                        ctx: response_ctx,
                        meta,
                        resp,
                    }
                    .into_response(),
                ),
            });

            let event = match result {
                Ok(event) => event,
                Err(error) => Event {
                    from_rt_id: Self::ID.to_string(),
                    event_type: EventType::TaskError(TaskError {
                        ctx,
                        meta: error_meta,
                        error: error.to_string(),
                    }),
                },
            };
            if let Err(error) = event_sender.send(event).await {
                wd_log::log_error_ln!("send MCP task result failed: {:?}", error);
            }
        });
        Ok(())
    }

    async fn exec(
        &self,
        task: TaskReq<McpRequest>,
    ) -> fae_agent::Result<TaskResp<McpResponse>> {
        let TaskReq { ctx, mut meta, req } = task;
        let resp = self.execute(req).await?;
        meta.publisher = Self::ID.to_string();
        Ok(TaskResp { ctx, meta, resp })
    }
}

async fn load_server_config(
    mcp_dir: &Path,
    server: &str,
) -> anyhow::Result<McpServerConfig> {
    anyhow::ensure!(!server.trim().is_empty(), "MCP server name must not be empty");
    let mut entries = tokio::fs::read_dir(mcp_dir)
        .await
        .map_err(|error| anyhow::anyhow!("MCP directory `{}`: {error}", mcp_dir.display()))?;
    let mut files = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if entry.file_type().await?.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("json")
        {
            files.push(path);
        }
    }
    files.sort();

    let mut found = None;
    for path in files {
        let content = tokio::fs::read_to_string(&path).await?;
        let config: McpConfigFile = serde_json::from_str(&content).map_err(|error| {
            anyhow::anyhow!("invalid MCP config `{}`: {error}", path.display())
        })?;
        if let Some(candidate) = config.mcp_servers.get(server) {
            anyhow::ensure!(
                found.is_none(),
                "MCP server `{server}` is defined more than once"
            );
            found = Some(candidate.clone());
        }
    }

    found.ok_or_else(|| anyhow::anyhow!("MCP server `{server}` was not found"))
}

async fn list_tools(
    server: &str,
    config: McpServerConfig,
) -> anyhow::Result<Vec<McpToolInfo>> {
    let tools = match config {
        McpServerConfig::Local { command, args, env } => {
            let mut command = Command::new(command);
            command.args(args).envs(env).stderr(Stdio::inherit());
            let transport = TokioChildProcess::new(command)?;
            let mut client = ().serve(transport).await?;
            let result = client.list_all_tools().await;
            let _ = client.close().await;
            result?
        }
        McpServerConfig::Remote { url, headers } => {
            let transport = remote_transport(url, headers)?;
            let mut client = ().serve(transport).await?;
            let result = client.list_all_tools().await;
            let _ = client.close().await;
            result?
        }
    };

    Ok(to_tool_info(server, tools))
}

async fn call_tool(
    config: McpServerConfig,
    request: McpRequest,
) -> anyhow::Result<McpResponse> {
    let arguments = serde_json::from_str::<Value>(&request.arguments)?;
    let Value::Object(arguments) = arguments else {
        anyhow::bail!("MCP tool arguments must be a JSON object");
    };
    let params = CallToolRequestParams::new(request.tool_name).with_arguments(arguments);

    let result = match config {
        McpServerConfig::Local { command, args, env } => {
            let mut command = Command::new(command);
            command.args(args).envs(env).stderr(Stdio::inherit());
            let transport = TokioChildProcess::new(command)?;
            let mut client = ().serve(transport).await?;
            let result = client.call_tool(params).await;
            let _ = client.close().await;
            result?
        }
        McpServerConfig::Remote { url, headers } => {
            let transport = remote_transport(url, headers)?;
            let mut client = ().serve(transport).await?;
            let result = client.call_tool(params).await;
            let _ = client.close().await;
            result?
        }
    };

    Ok(McpResponse {
        output: serde_json::to_string(&result)?,
    })
}

fn remote_transport(
    url: String,
    headers: HashMap<String, String>,
) -> anyhow::Result<StreamableHttpClientTransport<reqwest::Client>> {
    let headers = headers
        .into_iter()
        .map(|(name, value)| {
            Ok((
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(&value)?,
            ))
        })
        .collect::<anyhow::Result<HashMap<_, _>>>()?;
    let config = StreamableHttpClientTransportConfig::with_uri(url).custom_headers(headers);
    Ok(StreamableHttpClientTransport::with_client(
        reqwest::Client::new(),
        config,
    ))
}

fn to_tool_info(server: &str, tools: Vec<Tool>) -> Vec<McpToolInfo> {
    tools
        .into_iter()
        .map(|tool| McpToolInfo {
            server: server.to_string(),
            name: tool.name.into_owned(),
            description: tool.description.map(|value| value.into_owned()).unwrap_or_default(),
            input_schema: Value::Object((*tool.input_schema).clone()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loads_server_from_json_files() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "fae-mcp-runtime-{}-{}",
            std::process::id(),
            wd_tools::uuid::v4()
        ));
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(
            dir.join("servers.json"),
            r#"{"mcpServers":{"local":{"command":"echo","args":["ok"]}}}"#,
        )
        .await?;

        let config = load_server_config(&dir, "local").await?;
        assert_eq!(
            config,
            McpServerConfig::Local {
                command: "echo".into(),
                args: vec!["ok".into()],
                env: HashMap::new(),
            }
        );

        tokio::fs::remove_dir_all(dir).await?;
        Ok(())
    }
}
