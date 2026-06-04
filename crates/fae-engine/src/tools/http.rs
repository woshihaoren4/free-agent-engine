use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use serde_json::Value;

pub struct SendHttpRequest;

#[async_trait]
impl Tool for SendHttpRequest {
    fn name(&self) -> &str {
        "send_http_request"
    }

    fn description(&self) -> &str {
        "Send an HTTP request and return the response."
    }

    fn arguments(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "method": {
                    "type": "string",
                    "description": "The HTTP method (GET, POST, PUT, DELETE, etc.). Default is GET."
                },
                "url": {
                    "type": "string",
                    "description": "The URL to send the request to."
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as a JSON object."
                },
                "body": {
                    "type": "string",
                    "description": "Optional HTTP body as a string."
                }
            },
            "required": ["url"]
        })
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<String> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let method = args_val["method"].as_str().unwrap_or("GET");
        let url = args_val["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("url is required"))?;
        let headers = args_val["headers"].as_object();
        let body = args_val["body"].as_str();

        let client = reqwest::Client::new();
        let mut req = match method.to_uppercase().as_str() {
            "GET" => client.get(url),
            "POST" => client.post(url),
            "PUT" => client.put(url),
            "DELETE" => client.delete(url),
            "PATCH" => client.patch(url),
            _ => return Err(anyhow::anyhow!("Unsupported HTTP method")),
        };

        if let Some(hdrs) = headers {
            for (k, v) in hdrs {
                if let Some(v_str) = v.as_str() {
                    req = req.header(k, v_str);
                }
            }
        }

        if let Some(b) = body {
            req = req.body(b.to_string());
        }

        let resp = req.send().await?;
        let text = resp.text().await?;
        Ok(text)
    }
}
