use super::config::Config;
use larksuite_oapi_sdk_rs::service::im::v1::{
    CreateMessageReactionReqBody, CreateMessageReqBody, Emoji, ReplyMessageReqBody,
};
use larksuite_oapi_sdk_rs::{AccessTokenType, ApiReq, LarkClient, ReqBody, RequestOption};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::Instant};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MessageResource {
    pub message_id: String,
    pub chat_id: String,
    pub thread_id: String,
    pub root_id: String,
}
impl MessageResource {
    fn parse(value: &Value) -> anyhow::Result<Self> {
        let field = |name: &str| value[name].as_str().unwrap_or_default().to_owned();
        let resource = Self {
            message_id: field("message_id"),
            chat_id: field("chat_id"),
            thread_id: field("thread_id"),
            root_id: field("root_id"),
        };
        anyhow::ensure!(!resource.message_id.is_empty(), "Missing reply message ID");
        Ok(resource)
    }
    pub fn validate(&self, chat: &str, thread: Option<&str>) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.chat_id == chat && super::identity::valid_thread(&self.thread_id),
            "Reply thread/chat metadata unavailable or mismatched"
        );
        if let Some(thread) = thread {
            anyhow::ensure!(self.thread_id == thread, "Reply escaped the bound thread");
        }
        Ok(())
    }
}
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
    pub async fn withdraw(&self, message: &str) -> anyhow::Result<()> {
        anyhow::ensure!(super::identity::valid_thread(message), "Invalid message ID");
        match self
            .raw(
                http::Method::DELETE,
                &format!("/open-apis/im/v1/messages/{message}"),
                None,
            )
            .await
        {
            Ok(_) => Ok(()),
            // A lost successful response may leave cleanup pending after restart.
            Err(error)
                if error
                    .downcast_ref::<ApiFailure>()
                    .is_some_and(|e| e.code == 230011) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    pub async fn reply(
        &self,
        message: &str,
        chat: &str,
        kind: &str,
        content: String,
        uuid: &str,
        reply_in_thread: bool,
    ) -> anyhow::Result<String> {
        self.chat_pace(chat).await;
        self.pace().await;
        let body = ReplyMessageReqBody {
            content: Some(content.clone()),
            msg_type: Some(kind.into()),
            reply_in_thread: Some(reply_in_thread),
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
        if response.code_error.code != 230011 {
            return Err(ApiFailure {
                code: response.code_error.code as i64,
                message: response.code_error.msg.clone(),
            }
            .into());
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
    pub async fn message(&self, id: &str) -> anyhow::Result<MessageResource> {
        anyhow::ensure!(super::identity::valid_thread(id), "Invalid message ID");
        let value = self
            .raw(
                http::Method::GET,
                &format!("/open-apis/im/v1/messages/{id}"),
                None,
            )
            .await?;
        let item = value["data"]["items"]
            .as_array()
            .and_then(|items| items.iter().find(|m| m["message_id"].as_str() == Some(id)))
            .ok_or_else(|| anyhow::anyhow!("Message metadata unavailable"))?;
        MessageResource::parse(item)
    }
    pub async fn thread_reply(
        &self,
        message: &str,
        chat: &str,
        kind: &str,
        content: String,
        uuid: &str,
    ) -> anyhow::Result<MessageResource> {
        anyhow::ensure!(
            super::identity::valid_thread(message),
            "Invalid reply anchor"
        );
        self.chat_pace(chat).await;
        let value = self
            .raw(
                http::Method::POST,
                &format!("/open-apis/im/v1/messages/{message}/reply"),
                Some(json!({"content":content,"msg_type":kind,"reply_in_thread":true,"uuid":uuid})),
            )
            .await?;
        let mut result = MessageResource::parse(&value["data"])?;
        if result.thread_id.is_empty() || result.chat_id.is_empty() || result.root_id.is_empty() {
            // Preserve the returned ID even if metadata lookup is temporarily unavailable.
            if let Ok(queried) = self.message(&result.message_id).await {
                result = queried;
            }
        }
        Ok(result)
    }
    pub async fn reply_record(
        &self,
        store: &super::store::Store,
        id: &str,
        kind: &str,
        content: String,
        uuid: &str,
    ) -> anyhow::Result<String> {
        let record = store.get(id);
        if record.session_mapping_version == 1 || record.binding.thread_id.is_empty() {
            anyhow::ensure!(
                record.responses.is_empty() || record.session_mapping_version == 1,
                "Model output cannot be delivered without a ready thread"
            );
            return self
                .reply(
                    &record.message_id,
                    &record.binding.chat_id,
                    kind,
                    content,
                    uuid,
                    record.session_mapping_version == 1
                        && record.legacy_reply_in_thread.unwrap_or(false),
                )
                .await;
        }
        anyhow::ensure!(
            super::identity::owns_session(&record.binding, &record.session_id),
            "Invalid output thread binding"
        );
        let anchor = if record.reply_anchor_message_id.is_empty() {
            &record.message_id
        } else {
            &record.reply_anchor_message_id
        };
        let result = self
            .thread_reply(anchor, &record.binding.chat_id, kind, content.clone(), uuid)
            .await;
        let mut reply = match result {
            Ok(reply) => reply,
            Err(error)
                if error
                    .downcast_ref::<ApiFailure>()
                    .is_some_and(|e| e.code == 230011) =>
            {
                let binding = store.bind_thread(&record, &self.config)?;
                let mut replacement = None;
                for candidate in [
                    &record.thread_reply_message_id,
                    &binding.anchor,
                    &record.root_id,
                ] {
                    if candidate.is_empty() || candidate == anchor {
                        continue;
                    }
                    if let Ok(resource) = self.message(candidate).await {
                        if resource
                            .validate(&record.binding.chat_id, Some(&record.session_id))
                            .is_ok()
                        {
                            replacement = Some(candidate.clone());
                            break;
                        }
                    }
                }
                let replacement = replacement
                    .ok_or_else(|| anyhow::anyhow!("No live anchor in the bound thread"))?;
                store.update(id, |r| r.reply_anchor_message_id = replacement.clone())?;
                self.thread_reply(&replacement, &record.binding.chat_id, kind, content, uuid)
                    .await?
            }
            Err(error) => return Err(error),
        };
        if reply.thread_id.is_empty() || reply.chat_id.is_empty() {
            reply = self.message(&reply.message_id).await?;
        }
        reply.validate(&record.binding.chat_id, Some(&record.session_id))?;
        Ok(reply.message_id)
    }
    pub async fn text_record(
        &self,
        store: &super::store::Store,
        id: &str,
        text: &str,
        uuid: &str,
    ) -> anyhow::Result<String> {
        self.reply_record(store, id, "text", json!({"text":text}).to_string(), uuid)
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

#[cfg(test)]
pub mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    pub type Requests = Arc<std::sync::Mutex<Vec<(String, Value)>>>;
    pub async fn mock(
        mut handler: impl FnMut(&str, &Value) -> Value + Send + 'static,
    ) -> (String, Requests, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests: Requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (path, body) = loop {
                    let mut buffer = [0; 4096];
                    let size = socket.read(&mut buffer).await.unwrap();
                    if size == 0 {
                        break (String::new(), Value::Null);
                    }
                    bytes.extend_from_slice(&buffer[..size]);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length: usize = header
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            let path = header
                                .lines()
                                .next()
                                .unwrap()
                                .split_whitespace()
                                .nth(1)
                                .unwrap()
                                .to_owned();
                            let body = if length == 0 {
                                Value::Null
                            } else {
                                serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap()
                            };
                            break (path, body);
                        }
                    }
                };
                if path.is_empty() {
                    continue;
                }
                seen.lock().unwrap().push((path.clone(), body.clone()));
                let value = if path.contains("tenant_access_token") {
                    json!({"code":0,"tenant_access_token":"mock-token","expire":7200})
                } else {
                    handler(&path, &body)
                };
                let body = value.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (base, requests, task)
    }
}
