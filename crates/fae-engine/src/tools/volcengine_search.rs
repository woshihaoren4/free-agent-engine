use fae_agent::{Ctx, ToolRequest, ToolResponse, Tools};
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::Duration;

use super::{
    DEFAULT_CHANNEL, VOLCENGINE_SEARCH, effective_tool_name, ok_json, parse_arguments,
    request_tool_name, unsupported_tool,
};

const SEARCH_URL: &str = "https://open.feedcoopapi.com/search_api/web_search";
const API_KEY_ENV: &str = "ARK_WEB_SEARCH_APIKEY";

#[derive(Debug, Clone, Default)]
pub struct VolcengineSearchTool {
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    count: Option<u8>,
    filter: Option<SearchFilter>,
    time_range: Option<String>,
    query_rewrite: Option<bool>,
    content_formats: Option<ContentFormat>,
    industry: Option<Industry>,
    enable_waiting: Option<bool>,
    max_wait_time: Option<u32>,
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all(serialize = "PascalCase"), deny_unknown_fields)]
struct SearchFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    need_content: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    need_url: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sites: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    block_hosts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_info_level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icp_host_only: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum ContentFormat {
    Text,
    Markdown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Industry {
    Finance,
    Game,
    Health,
    Gov,
}

impl SearchArgs {
    fn validate(&self) -> Result<(), String> {
        if self.query.trim().is_empty() || self.query.chars().count() > 100 {
            return Err("query must contain 1 to 100 characters".into());
        }
        if self.count.is_some_and(|count| !(1..=50).contains(&count)) {
            return Err("count must be between 1 and 50".into());
        }
        if self.timeout_secs == Some(0) {
            return Err("timeout_secs must be at least 1".into());
        }
        if self.max_wait_time.is_some_and(|time| time > 10_000) {
            return Err("max_wait_time must not exceed 10000 milliseconds".into());
        }
        if let Some(filter) = &self.filter {
            if filter.auth_info_level.is_some_and(|level| level > 1) {
                return Err("filter.auth_info_level must be 0 or 1".into());
            }
            for (name, sites, limit) in [
                ("sites", &filter.sites, 20),
                ("block_hosts", &filter.block_hosts, 5),
            ] {
                if let Some(sites) = sites {
                    let hosts: Vec<_> = sites.split('|').collect();
                    if hosts.len() > limit || hosts.iter().any(|host| host.trim().is_empty()) {
                        return Err(format!(
                            "filter.{name} must contain 1 to {limit} domains separated by |"
                        ));
                    }
                }
            }
        }
        if let Some(range) = &self.time_range {
            if !matches!(
                range.as_str(),
                "OneDay" | "OneWeek" | "OneMonth" | "OneYear"
            ) {
                let valid = range.split_once("..").is_some_and(|(start, end)| {
                    let parse = |date: &str| {
                        chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                            .ok()
                            .filter(|parsed| parsed.format("%Y-%m-%d").to_string() == date)
                    };
                    matches!((parse(start), parse(end)), (Some(start), Some(end)) if start <= end)
                });
                if !valid {
                    return Err(
                        "time_range must be OneDay, OneWeek, OneMonth, OneYear, or YYYY-MM-DD..YYYY-MM-DD with start <= end"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    fn request_body(&self) -> Value {
        let mut body = json!({
            "Query": self.query,
            "SearchType": "web",
            "Count": self.count.unwrap_or(10),
        });
        for (name, value) in [
            ("Filter", json!(self.filter)),
            ("TimeRange", json!(self.time_range)),
            (
                "QueryControl",
                self.query_rewrite
                    .map(|rewrite| json!({ "QueryRewrite": rewrite }))
                    .unwrap_or(Value::Null),
            ),
            ("ContentFormats", json!(self.content_formats)),
            ("Industry", json!(self.industry)),
            ("EnableWaiting", json!(self.enable_waiting)),
            ("MaxWaitTime", json!(self.max_wait_time)),
        ] {
            if !value.is_null() {
                body[name] = value;
            }
        }
        body
    }
}

#[async_trait::async_trait]
impl Tools for VolcengineSearchTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != VOLCENGINE_SEARCH {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": VOLCENGINE_SEARCH,
            "description": "Search the web using Volcengine Doubao Search Custom. Returns the complete API JSON, including titles, URLs, Summary, Content, publication times and response metadata. Prefer Summary or Content over Snippet for answering questions. Authentication uses the ARK_WEB_SEARCH_APIKEY environment variable.",
            "parameters": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": { "type": "string", "minLength": 1, "maxLength": 100, "description": "Search query." },
                    "count": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Number of results. Defaults to 10." },
                    "filter": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "need_content": { "type": "boolean", "description": "Only return results with full content." },
                            "need_url": { "type": "boolean", "description": "Only return results with original URLs." },
                            "sites": { "type": "string", "description": "Up to 20 full domains separated by |, e.g. aliyun.com|mp.qq.com." },
                            "block_hosts": { "type": "string", "description": "Up to 5 blocked domains separated by |." },
                            "auth_info_level": { "type": "integer", "enum": [0, 1], "description": "0: unrestricted; 1: highly authoritative sources only." },
                            "icp_host_only": { "type": "boolean", "description": "Only search websites with Chinese ICP registration." }
                        }
                    },
                    "time_range": { "type": "string", "description": "Publication time: OneDay, OneWeek, OneMonth, OneYear, or YYYY-MM-DD..YYYY-MM-DD (inclusive). Omit for unrestricted." },
                    "query_rewrite": { "type": "boolean", "description": "Enable query rewriting. Defaults to false; may increase latency." },
                    "content_formats": { "type": "string", "enum": ["text", "markdown"], "description": "Full content format. Defaults to text." },
                    "industry": { "type": "string", "enum": ["finance", "game", "health", "gov"], "description": "Restrict search to an industry." },
                    "enable_waiting": { "type": "boolean", "description": "Queue requests exceeding the QPS limit. Defaults to false." },
                    "max_wait_time": { "type": "integer", "minimum": 0, "maximum": 10000, "description": "Maximum queue wait in milliseconds. Defaults to 5000 when queuing is enabled." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "description": "Request timeout in seconds. Defaults to 60." }
                },
                "required": ["query"]
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != VOLCENGINE_SEARCH {
            return Err(unsupported_tool(req.get_tool_name()));
        }
        let args: SearchArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(response) => return Ok(response),
        };
        if let Err(message) = args.validate() {
            return Ok(ToolResponse::with_error(400, message));
        }

        let api_key = match std::env::var(API_KEY_ENV) {
            Ok(key) if !key.trim().is_empty() => key,
            _ => {
                return Ok(ToolResponse::with_error(
                    500,
                    format!("{API_KEY_ENV} is missing or empty"),
                ));
            }
        };
        let mut authorization = match HeaderValue::from_str(&format!("Bearer {}", api_key.trim())) {
            Ok(value) => value,
            Err(_) => {
                return Ok(ToolResponse::with_error(
                    500,
                    format!("{API_KEY_ENV} contains invalid header characters"),
                ));
            }
        };
        authorization.set_sensitive(true);

        let response = match self
            .client
            .post(SEARCH_URL)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .timeout(Duration::from_secs(args.timeout_secs.unwrap_or(60)))
            .json(&args.request_body())
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => return Ok(ToolResponse::with_error(502, error.to_string())),
        };
        let status = response.status();
        let body: Value = match response.json().await {
            Ok(body) => body,
            Err(error) => {
                return Ok(ToolResponse::with_error(
                    502,
                    format!("search API returned HTTP {status} with invalid JSON: {error}"),
                ));
            }
        };
        if !status.is_success() {
            return Ok(ToolResponse::with_error(
                status.as_u16().into(),
                serde_json::to_string(&body)?,
            ));
        }
        if body
            .pointer("/ResponseMetadata/Error")
            .is_some_and(|error| !error.is_null())
        {
            return Ok(ToolResponse::with_error(502, serde_json::to_string(&body)?));
        }
        ok_json(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_maps_arguments_to_api_json() {
        let cases = [
            (
                "default parameters",
                json!({ "query": "火山引擎" }),
                json!({ "Query": "火山引擎", "SearchType": "web", "Count": 10 }),
            ),
            (
                "all optional parameters",
                json!({
                    "query": "火山引擎最新动态",
                    "count": 5,
                    "filter": {
                        "need_content": true,
                        "need_url": true,
                        "sites": "volcengine.com|example.com",
                        "block_hosts": "blocked.example.com",
                        "auth_info_level": 1,
                        "icp_host_only": true
                    },
                    "time_range": "OneWeek",
                    "query_rewrite": true,
                    "content_formats": "markdown",
                    "industry": "gov",
                    "enable_waiting": true,
                    "max_wait_time": 10000,
                    "timeout_secs": 30
                }),
                json!({
                    "Query": "火山引擎最新动态",
                    "SearchType": "web",
                    "Count": 5,
                    "Filter": {
                        "NeedContent": true,
                        "NeedUrl": true,
                        "Sites": "volcengine.com|example.com",
                        "BlockHosts": "blocked.example.com",
                        "AuthInfoLevel": 1,
                        "IcpHostOnly": true
                    },
                    "TimeRange": "OneWeek",
                    "QueryControl": { "QueryRewrite": true },
                    "ContentFormats": "markdown",
                    "Industry": "gov",
                    "EnableWaiting": true,
                    "MaxWaitTime": 10000
                }),
            ),
            (
                "explicit false values and omitted filter fields",
                json!({
                    "query": "火山引擎",
                    "filter": { "need_content": false, "need_url": false },
                    "query_rewrite": false,
                    "enable_waiting": false
                }),
                json!({
                    "Query": "火山引擎",
                    "SearchType": "web",
                    "Count": 10,
                    "Filter": { "NeedContent": false, "NeedUrl": false },
                    "QueryControl": { "QueryRewrite": false },
                    "EnableWaiting": false
                }),
            ),
        ];

        for (name, arguments, expected) in cases {
            let args: SearchArgs = serde_json::from_value(arguments).unwrap();
            assert_eq!(args.request_body(), expected, "{name}");
        }
    }
}
