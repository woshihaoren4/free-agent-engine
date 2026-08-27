use std::collections::HashMap;

use fae_agent::{Ctx, ToolRequest, ToolResponse, Tools};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::Duration;

use super::{
    DEFAULT_CHANNEL, SEND_HTTP_REQUEST, effective_tool_name, ok_json, parse_arguments,
    request_tool_name, unsupported_tool,
};

#[derive(Debug, Clone)]
pub struct SendHttpRequestTool {
    client: reqwest::Client,
}

impl Default for SendHttpRequestTool {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SendHttpRequestArgs {
    url: String,
    method: Option<String>,
    headers: Option<HashMap<String, String>>,
    body: Option<String>,
    json: Option<Value>,
    timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SendHttpRequestResult {
    url: String,
    status: u16,
    success: bool,
    headers: HashMap<String, String>,
    body: String,
}

#[async_trait::async_trait]
impl Tools for SendHttpRequestTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != SEND_HTTP_REQUEST {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": SEND_HTTP_REQUEST,
            "description": "Send an HTTP request and return response status, headers, and body text.",
            "parameters": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Request URL." },
                    "method": { "type": "string", "description": "HTTP method. Defaults to GET." },
                    "headers": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Optional request headers."
                    },
                    "body": { "type": "string", "description": "Optional raw request body." },
                    "json": { "description": "Optional JSON request body. Takes precedence over body." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "description": "Optional timeout in seconds. Defaults to 60." }
                },
                "required": ["url"]
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != SEND_HTTP_REQUEST {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: SendHttpRequestArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };

        let method = args
            .method
            .unwrap_or_else(|| "GET".to_string())
            .parse::<reqwest::Method>();
        let method = match method {
            Ok(method) => method,
            Err(err) => return Ok(ToolResponse::with_error(400, err.to_string())),
        };

        let headers = match parse_headers(args.headers.as_ref()) {
            Ok(headers) => headers,
            Err(err) => return Ok(ToolResponse::with_error(400, err.to_string())),
        };

        let mut request = self
            .client
            .request(method, &args.url)
            .headers(headers)
            .timeout(Duration::from_secs(args.timeout_secs.unwrap_or(60)));

        if let Some(json_body) = args.json {
            request = request.json(&json_body);
        } else if let Some(body) = args.body {
            request = request.body(body);
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => return Ok(ToolResponse::with_error(500, err.to_string())),
        };

        let status = response.status();
        let headers = response_headers(response.headers());
        let body = match response.text().await {
            Ok(body) => body,
            Err(err) => return Ok(ToolResponse::with_error(500, err.to_string())),
        };

        ok_json(SendHttpRequestResult {
            url: args.url,
            status: status.as_u16(),
            success: status.is_success(),
            headers,
            body,
        })
    }
}

fn parse_headers(headers: Option<&HashMap<String, String>>) -> anyhow::Result<HeaderMap> {
    let mut parsed = HeaderMap::new();

    for (name, value) in headers.into_iter().flatten() {
        parsed.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }

    Ok(parsed)
}

fn response_headers(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}
