use super::config::Config;
use larksuite_oapi_sdk_rs::service::im::v1::{
    CreateMessageReactionReqBody, CreateMessageReqBody, Emoji, ReplyMessageReqBody,
};
use larksuite_oapi_sdk_rs::{AccessTokenType, ApiReq, LarkClient, ReqBody, RequestOption};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::Instant};

#[derive(Debug)]
pub struct ApiFailure {
    pub code: i64,
    pub message: String,
}
impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Feishu API error {}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiFailure {}
impl ApiFailure {
    pub fn streaming_closed(&self) -> bool {
        let message = self.message.to_lowercase();
        message.contains("stream")
            && (message.contains("close")
                || message.contains("timeout")
                || message.contains("expired"))
    }
}

#[derive(Clone)]
pub struct Sdk {
    pub client: LarkClient,
    rate: Arc<Mutex<Instant>>,
    chats: Arc<Mutex<HashMap<String, Instant>>>,
    pub config: Config,
}
impl Sdk {
    #[cfg(test)]
    pub fn for_test(config: Config, base: String) -> Self {
        Self {
            client: LarkClient::builder(&config.app_id, &config.app_secret)
                .base_url(base)
                .max_retries(1)
                .build()
                .unwrap(),
            rate: Arc::new(Mutex::new(Instant::now())),
            chats: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }
    pub fn new(config: Config) -> anyhow::Result<Self> {
        Ok(Self {
            client: LarkClient::builder(&config.app_id, &config.app_secret)
                .base_url("https://open.feishu.cn")
                // 0.3.13 counts total attempts (0..max_retries), not extra retries.
                .max_retries(1)
                .timeout(Duration::from_secs(10))
                .build()?,
            rate: Arc::new(Mutex::new(Instant::now())),
            chats: Arc::new(Mutex::new(HashMap::new())),
            config,
        })
    }
    async fn pace(&self) {
        let mut next = self.rate.lock().await;
        tokio::time::sleep_until(*next).await;
        *next = Instant::now() + Duration::from_millis(75);
    }
    async fn chat_pace(&self, chat: &str) {
        let mut chats = self.chats.lock().await;
        let next = chats.entry(chat.to_owned()).or_insert_with(Instant::now);
        tokio::time::sleep_until(*next).await;
        *next = Instant::now() + Duration::from_millis(220);
    }
    pub async fn raw(
        &self,
        method: http::Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let mut req = ApiReq::new(method, path);
        req.supported_access_token_types = vec![AccessTokenType::Tenant];
        req.body = body.as_ref().map(ReqBody::json).transpose()?;
        self.pace().await;
        let response = self
            .client
            .raw_request(&req, &RequestOption::default())
            .await?;
        let value: Value = serde_json::from_slice(&response.raw_body)?;
        if let Some(delay) = response
            .header
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            *self.rate.lock().await = Instant::now() + Duration::from_secs(delay.min(120));
        }
        if response.status_code != 200 || value["code"].as_i64() != Some(0) {
            return Err(ApiFailure {
                code: value["code"]
                    .as_i64()
                    .unwrap_or(response.status_code as i64),
                message: value["msg"].as_str().unwrap_or("request rejected").into(),
            }
            .into());
        }
        Ok(value)
    }
    pub async fn bot_id(&self) -> anyhow::Result<String> {
        let value = self
            .raw(http::Method::GET, "/open-apis/bot/v3/info", None)
            .await?;
        if let Some(app) = value["bot"]["app_id"].as_str() {
            anyhow::ensure!(app == self.config.app_id, "Bot application mismatch");
        }
        value["bot"]["open_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Bot open ID unavailable"))
    }
    pub async fn typing(&self, message: &str) -> anyhow::Result<String> {
        self.pace().await;
        let mut emoji = Emoji::default();
        emoji.emoji_type = Some("Typing".into());
        let response = self
            .client
            .im()
            .message_reaction
            .create(
                message,
                &CreateMessageReactionReqBody {
                    reaction_type: Some(emoji),
                },
                &RequestOption::default(),
            )
            .await?;
        anyhow::ensure!(
            response.success(),
            "Typing reaction rejected: {}",
            response.code_error
        );
        response
            .data
            .and_then(|d| d.reaction_id)
            .ok_or_else(|| anyhow::anyhow!("Missing reaction ID"))
    }
    pub async fn untype(&self, message: &str, reaction: &str) -> anyhow::Result<()> {
        self.pace().await;
        let response = self
            .client
            .im()
            .message_reaction
            .delete(message, reaction, &RequestOption::default())
            .await?;
        anyhow::ensure!(
            response.success(),
            "Reaction deletion rejected: {}",
            response.code_error
        );
        Ok(())
    }
    pub async fn reply(
        &self,
        message: &str,
        chat: &str,
        kind: &str,
        content: String,
        uuid: &str,
    ) -> anyhow::Result<String> {
        self.chat_pace(chat).await;
        self.pace().await;
        let body = ReplyMessageReqBody {
            content: Some(content.clone()),
            msg_type: Some(kind.into()),
            reply_in_thread: Some(self.config.group.reply_in_thread),
            uuid: Some(uuid.into()),
        };
        let response = self
            .client
            .im()
            .message
            .reply(message, &body, &RequestOption::default())
            .await?;
        if response.success() {
            return response
                .data
                .and_then(|d| d.message_id)
                .ok_or_else(|| anyhow::anyhow!("Missing reply ID"));
        }
        // Only a known withdrawn/unavailable reply anchor permits chat fallback.
        if !matches!(response.code_error.code, 230011 | 230020) {
            anyhow::bail!("Reply rejected: {}", response.code_error);
        }
        self.chat_pace(chat).await;
        self.pace().await;
        let response = self
            .client
            .im()
            .message
            .create(
                "chat_id",
                &CreateMessageReqBody {
                    receive_id: Some(chat.into()),
                    msg_type: Some(kind.into()),
                    content: Some(content),
                    uuid: Some(uuid.into()),
                },
                &RequestOption::default(),
            )
            .await?;
        anyhow::ensure!(
            response.success(),
            "Chat reply rejected: {}",
            response.code_error
        );
        response
            .data
            .and_then(|d| d.message_id)
            .ok_or_else(|| anyhow::anyhow!("Missing reply ID"))
    }
    pub async fn text(
        &self,
        message: &str,
        chat: &str,
        text: &str,
        uuid: &str,
    ) -> anyhow::Result<String> {
        self.reply(
            message,
            chat,
            "text",
            json!({"text": text}).to_string(),
            uuid,
        )
        .await
    }
    pub fn sanitize(&self, error: &str) -> String {
        let safe = error.replace(&self.config.app_secret, "[redacted]");
        let mut result = String::new();
        for word in safe.split_whitespace() {
            if word.starts_with("sk-") || word.contains("access_token") || word.contains("Bearer") {
                result.push_str("[redacted] ");
            } else {
                result.push_str(word);
                result.push(' ');
            }
            if result.len() >= 1500 {
                break;
            }
        }
        result.trim().to_owned()
    }
}
