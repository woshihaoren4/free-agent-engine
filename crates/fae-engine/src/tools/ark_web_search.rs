use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use serde_json::Value;
use fae_agent::ToolResponse;

#[derive(Debug)]
pub struct ArkWebSearch;

#[async_trait]
impl Tool for ArkWebSearch {
    fn name(&self) -> &str {
        "ark_web_search"
    }

    fn description(&self) -> &str {
        "A web and image search tool powered by Volcano Engine Ark. Allows searching for web pages or images."
    }

    //因为需要服务，这里没加 web_summary
    fn arguments(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "用户搜索query，1~100个字符(过长会截断)，不支持多词搜索"
                },
                "search_type": {
                    "type": "string",
                    "enum": ["web", "image"],
                    "description": "搜索类型枚举值。web：返回搜索到的站点信息；image：图片搜索"
                },
                "count": {
                    "type": "integer",
                    "description": "返回条数。web最多50条(默认10条)，image最多5条"
                },
                "filter": {
                    "type": "object",
                    "description": "过滤条件",
                    "properties": {
                        "need_content": { "type": "boolean", "description": "是否仅返回有正文的结果，默认false" },
                        "need_url": { "type": "boolean", "description": "是否仅返回原文链接的结果，默认false" },
                        "sites": { "type": "string", "description": "指定搜索的站点范围，多个站点使用'|'分隔，最多支持20个。需填入完整域名，示例：aliyun.com|mp.qq.com" },
                        "block_hosts": { "type": "string", "description": "指定屏蔽的搜索Site，多个域名使用'|'分隔，最多支持5个。需填入完整域名" },
                        "auth_info_level": { "type": "integer", "description": "指定仅在非常权威内容范围内搜索，默认为0。0:不限制, 1:限制非常权威" },
                        "image_width_min": { "type": "integer", "description": "最小宽度" },
                        "image_height_min": { "type": "integer", "description": "最小高度" },
                        "image_width_max": { "type": "integer", "description": "最大宽度" },
                        "image_height_max": { "type": "integer", "description": "最大高度" },
                        "image_shapes": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "允许的形状，枚举值：横长方形、竖长方形、方形"
                        }
                    }
                },
                "need_summary": {
                    "type": "boolean",
                    "description": "是否需要搜索结果 WebItem 中的相关摘要，默认false。调用 web_summary 时，本字段必须为true"
                },
                "time_range": {
                    "type": "string",
                    "description": "指定搜索的发文时间。枚举值：OneDay, OneWeek, OneMonth, OneYear，或 YYYY-MM-DD..YYYY-MM-DD"
                },
                "query_control": {
                    "type": "object",
                    "properties": {
                        "query_rewrite": { "type": "boolean", "description": "是否开启Query改写，默认false" }
                    }
                },
                "content_formats": {
                    "type": "string",
                    "description": "指定返回正文的格式，默认为Text。text：text格式；markdown：markdown格式"
                },
                "industry": {
                    "type": "string",
                    "description": "执行行业类型搜索，支持 finance(金融), game(电子游戏)"
                }
            },
            "required": ["query", "search_type"]
        })
    }

    async fn call(&self, _iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let query = args_val["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("query is required"))?;
        let search_type = args_val["search_type"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("search_type is required"))?;

        let api_key = std::env::var("ARK_WEB_SEARCH_APIKEY").map_err(|_| {
            anyhow::anyhow!("ARK_WEB_SEARCH_APIKEY environment variable is not set")
        })?;

        let url = "https://open.feedcoopapi.com/search_api/web_search";

        let mut req_body = serde_json::json!({
            "Query": query,
            "SearchType": search_type,
        });

        if let Some(c) = args_val.get("count").and_then(|v| v.as_i64()) {
            req_body["Count"] = serde_json::json!(c);
        }

        if let Some(filter) = args_val.get("filter").and_then(|v| v.as_object()) {
            let mut req_filter = serde_json::Map::new();
            if let Some(v) = filter.get("need_content") {
                req_filter.insert("NeedContent".to_string(), v.clone());
            }
            if let Some(v) = filter.get("need_url") {
                req_filter.insert("NeedUrl".to_string(), v.clone());
            }
            if let Some(v) = filter.get("sites") {
                req_filter.insert("Sites".to_string(), v.clone());
            }
            if let Some(v) = filter.get("block_hosts") {
                req_filter.insert("BlockHosts".to_string(), v.clone());
            }
            if let Some(v) = filter.get("auth_info_level") {
                req_filter.insert("AuthInfoLevel".to_string(), v.clone());
            }
            if let Some(v) = filter.get("image_width_min") {
                req_filter.insert("ImageWidthMin".to_string(), v.clone());
            }
            if let Some(v) = filter.get("image_height_min") {
                req_filter.insert("ImageHeightMin".to_string(), v.clone());
            }
            if let Some(v) = filter.get("image_width_max") {
                req_filter.insert("ImageWidthMax".to_string(), v.clone());
            }
            if let Some(v) = filter.get("image_height_max") {
                req_filter.insert("ImageHeightMax".to_string(), v.clone());
            }
            if let Some(v) = filter.get("image_shapes") {
                req_filter.insert("ImageShapes".to_string(), v.clone());
            }
            if !req_filter.is_empty() {
                req_body["Filter"] = serde_json::Value::Object(req_filter);
            }
        }

        if let Some(v) = args_val.get("need_summary") {
            req_body["NeedSummary"] = v.clone();
        }
        if let Some(v) = args_val.get("time_range") {
            req_body["TimeRange"] = v.clone();
        }
        if let Some(query_control) = args_val.get("query_control").and_then(|v| v.as_object()) {
            if let Some(v) = query_control.get("query_rewrite") {
                req_body["QueryControl"] = serde_json::json!({
                    "QueryRewrite": v.clone()
                });
            }
        }
        if let Some(v) = args_val.get("content_formats") {
            req_body["ContentFormats"] = v.clone();
        }
        if let Some(v) = args_val.get("industry") {
            req_body["Industry"] = v.clone();
        }

        let client = reqwest::Client::new();
        let resp = client
            .post(url)
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&req_body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Ark Web Search API request failed with status: {} body: {}",
                status,
                err_text
            ));
        }

        let text = resp.text().await?;
        Ok(ToolResponse::with_result(text))
    }
}

impl Default for ArkWebSearch {
    fn default() -> Self {
        Self
    }
}
