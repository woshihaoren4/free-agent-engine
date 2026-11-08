use super::{
    sdk::Sdk,
    store::{State, Store},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Serialize, Deserialize)]
pub struct Card {
    pub response_id: String,
    pub text: String,
    pub ended: bool,
    pub card_id: Option<String>,
    pub reply_id: Option<String>,
    pub reply_uuid: String,
    pub reply_attempt: Option<i64>,
    pub sequence: u64,
    pub sent_text: String,
    pub closed: bool,
    pub degraded: bool,
    #[serde(default)]
    pub reopened: bool,
    pub pending: Option<Mutation>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Mutation {
    pub sequence: u64,
    pub uuid: String,
    pub kind: String,
    pub body: Value,
}
impl Card {
    pub fn new(response_id: String) -> Self {
        Self {
            response_id,
            text: String::new(),
            ended: false,
            card_id: None,
            reply_id: None,
            reply_uuid: wd_tools::uuid::v4(),
            reply_attempt: None,
            sequence: 0,
            sent_text: String::new(),
            closed: false,
            degraded: false,
            reopened: false,
            pending: None,
        }
    }
}
pub fn document() -> anyhow::Result<Value> {
    let value = json!({
        "schema":"2.0",
        "config":{"streaming_mode":true,"update_multi":true,
            "summary":{"content":"生成中"},
            "streaming_config":{"print_frequency_ms":{"default":50},"print_step":{"default":1}}},
        "body":{"elements":[
            {"tag":"markdown","content":"生成中…","element_id":"status"},
            {"tag":"markdown","content":"","element_id":"answer"}
        ]}
    });
    let card: larksuite_oapi_sdk_rs::card::v2::Card = serde_json::from_value(value.clone())?;
    larksuite_oapi_sdk_rs::card::cardkit::CardDocument::new(card)?;
    Ok(value)
}

pub struct Writer {
    pub sdk: Sdk,
    pub store: Arc<Store>,
}
impl Writer {
    fn change(&self, id: &str, index: usize, change: impl FnOnce(&mut Card)) -> anyhow::Result<()> {
        self.store.update(id, |r| change(&mut r.responses[index]))?;
        Ok(())
    }
    async fn mutation(
        &self,
        id: &str,
        index: usize,
        kind: &str,
        mut body: Value,
    ) -> anyhow::Result<()> {
        let card = self.store.get(id).responses[index].clone();
        let mutation = match card.pending {
            Some(mutation) => mutation,
            None => {
                let sequence = card.sequence + 1;
                let uuid = wd_tools::uuid::v4();
                body["sequence"] = sequence.into();
                body["uuid"] = uuid.clone().into();
                let mutation = Mutation {
                    sequence,
                    uuid,
                    kind: kind.into(),
                    body,
                };
                self.change(id, index, |c| c.pending = Some(mutation.clone()))?;
                mutation
            }
        };
        let card_id = card
            .card_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Card ID missing"))?;
        let (method, path) = match mutation.kind.as_str() {
            "content" => (
                http::Method::PUT,
                format!("/open-apis/cardkit/v1/cards/{card_id}/elements/answer/content"),
            ),
            "status" => (
                http::Method::PUT,
                format!("/open-apis/cardkit/v1/cards/{card_id}/elements/status/content"),
            ),
            _ => (
                http::Method::PATCH,
                format!("/open-apis/cardkit/v1/cards/{card_id}/settings"),
            ),
        };
        let result = self
            .sdk
            .raw(method, &path, Some(mutation.body.clone()))
            .await;
        if let Err(error) = result {
            if !card.reopened
                && mutation.kind == "content"
                && error
                    .downcast_ref::<super::sdk::ApiFailure>()
                    .is_some_and(|e| e.streaming_closed())
            {
                let sequence = mutation.sequence + 1;
                let uuid = wd_tools::uuid::v4();
                let reopen = Mutation {
                    sequence,
                    uuid: uuid.clone(),
                    kind: "reopen".into(),
                    body: json!({"sequence":sequence,"uuid":uuid,"settings":json!({"streaming_mode":true}).to_string()}),
                };
                // The content update was explicitly rejected; reopen gets a fresh sequence.
                self.change(id, index, |c| {
                    c.pending = Some(reopen);
                })?;
            }
            return Err(error);
        }
        self.change(id, index, |c| {
            c.sequence = mutation.sequence;
            if mutation.kind == "content" {
                c.sent_text = mutation.body["content"].as_str().unwrap_or_default().into();
            }
            if mutation.kind == "close" {
                c.closed = true;
            }
            if mutation.kind == "reopen" {
                c.reopened = true;
            }
            c.pending = None;
        })
    }
    pub async fn flush(&self, id: &str) -> anyhow::Result<()> {
        let record = self.store.get(id);
        for index in 0..record.responses.len() {
            let mut card = self.store.get(id).responses[index].clone();
            if card.text.trim().is_empty() || card.closed {
                continue;
            }
            if card.degraded {
                if card.ended || record.outcome.is_some() {
                    let now = chrono::Utc::now().timestamp_millis();
                    anyhow::ensure!(
                        card.reply_attempt.is_none_or(|t| now - t < 3600000),
                        "Text delivery acceptance unknown beyond dedup window"
                    );
                    self.change(id, index, |c| {
                        c.reply_attempt.get_or_insert(now);
                    })?;
                    let text = format!("流式回复失败，以下为完整结果：\n{}", card.text);
                    let reply = self
                        .sdk
                        .text(
                            &record.message_id,
                            &record.binding.chat_id,
                            &text,
                            &card.reply_uuid,
                        )
                        .await?;
                    self.change(id, index, |c| {
                        c.reply_id = Some(reply);
                        c.closed = true;
                    })?;
                }
                continue;
            }
            if card.card_id.is_none() {
                let value = self
                    .sdk
                    .raw(
                        http::Method::POST,
                        "/open-apis/cardkit/v1/cards",
                        Some(json!({
                            "type":"card_json","data":document()?.to_string()
                        })),
                    )
                    .await?;
                let card_id = value["data"]["card_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Card ID missing"))?
                    .to_owned();
                self.change(id, index, |c| c.card_id = Some(card_id))?;
                card = self.store.get(id).responses[index].clone();
            }
            if card.reply_id.is_none() {
                let now = chrono::Utc::now().timestamp_millis();
                anyhow::ensure!(
                    card.reply_attempt.is_none_or(|t| now - t < 3600000),
                    "Reply acceptance unknown beyond platform dedup window; manual delivery required"
                );
                self.change(id, index, |c| {
                    c.reply_attempt.get_or_insert(now);
                })?;
                let reply = self
                    .sdk
                    .reply(
                        &record.message_id,
                        &record.binding.chat_id,
                        "interactive",
                        json!({
                            "type":"card","data":{"card_id":card.card_id}
                        })
                        .to_string(),
                        &card.reply_uuid,
                    )
                    .await?;
                self.change(id, index, |c| c.reply_id = Some(reply))?;
            }
            if card.pending.is_some() {
                self.mutation(id, index, "", Value::Null).await?;
                if self.store.get(id).responses[index].closed {
                    continue;
                }
            }
            let displayed = display_page(&self.store.get(id).responses, index);
            if displayed != self.store.get(id).responses[index].sent_text {
                self.mutation(id, index, "content", json!({"content":displayed}))
                    .await?;
            }
            if card.ended || record.outcome.is_some() {
                let status = if record.outcome.as_deref() == Some("failed") {
                    "未完成"
                } else if record.outcome.as_deref() == Some("cancelled") {
                    "已取消"
                } else if card.ended {
                    "中间回复"
                } else {
                    "已完成"
                };
                self.mutation(id, index, "status", json!({"content":status}))
                    .await?;
                self.mutation(id, index, "close", json!({
                    "settings":json!({"streaming_mode":false,"summary":{"content":status}}).to_string()
                })).await?;
            }
        }
        let record = self.store.get(id);
        if let Some(notice) = &record.notice {
            if !record.notice_sent {
                let now = chrono::Utc::now().timestamp_millis();
                anyhow::ensure!(
                    record.notice_attempt.is_none_or(|t| now - t < 3600000),
                    "Notice delivery acceptance unknown beyond dedup window"
                );
                self.store.update(id, |r| {
                    r.notice_attempt.get_or_insert(now);
                })?;
                self.sdk
                    .text(
                        &record.message_id,
                        &record.binding.chat_id,
                        notice,
                        &record.notice_uuid,
                    )
                    .await?;
                self.store.update(id, |r| r.notice_sent = true)?;
            }
        }
        if record.outcome.is_some()
            && record
                .responses
                .iter()
                .all(|c| c.closed || c.text.trim().is_empty())
            && (record.notice.is_none() || self.store.get(id).notice_sent)
        {
            if let Some(reaction) = &record.reaction_id {
                self.sdk.untype(&record.message_id, reaction).await?;
            }
            self.store.update(id, |r| {
                r.state = State::Delivered;
                r.delivered_at = Some(chrono::Utc::now().timestamp_millis());
                r.reaction_id = None;
                r.last_error = None;
            })?;
        }
        Ok(())
    }
}

/// Split on Unicode boundaries using encoded JSON size, leaving room for card metadata.
pub fn pages(text: &str) -> Vec<String> {
    let mut pages = Vec::new();
    let mut page = String::new();
    let mut size = 2usize;
    for ch in text.chars() {
        let cost = serde_json::to_string(&ch.to_string()).unwrap().len() - 2;
        if size + cost > 24000 {
            let boundary = page
                .rfind('\n')
                .filter(|at| page.len() - at < 2048)
                .map(|at| at + 1)
                .unwrap_or(page.len());
            let tail = page.split_off(boundary);
            pages.push(std::mem::replace(&mut page, tail));
            size = serde_json::to_string(&page).unwrap().len();
        }
        page.push(ch);
        size += cost;
    }
    if !page.is_empty() {
        pages.push(page);
    }
    pages
}

fn display_page(cards: &[Card], index: usize) -> String {
    let card = &cards[index];
    let mut fence: Option<String> = None;
    for previous in cards
        .iter()
        .take(index)
        .filter(|c| c.response_id == card.response_id)
    {
        for line in previous.text.lines() {
            if line.trim_start().starts_with("```") {
                fence = if fence.is_some() {
                    None
                } else {
                    Some(line.to_owned())
                };
            }
        }
    }
    let mut rendered = fence.as_ref().map(|f| format!("{f}\n")).unwrap_or_default();
    rendered.push_str(&card.text);
    for line in card.text.lines() {
        if line.trim_start().starts_with("```") {
            fence = if fence.is_some() {
                None
            } else {
                Some(line.to_owned())
            };
        }
    }
    if fence.is_some() {
        rendered.push_str("\n```");
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn retries_mutations_with_same_sequence_and_closes_only_after_finished() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, mut calls) = tokio::sync::mpsc::channel(32);
        let server = tokio::spawn(async move {
            let mut failed_once = false;
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
                        let header = String::from_utf8_lossy(&bytes[..end]).to_owned();
                        let length: usize = header
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            let body = if length == 0 {
                                Value::Null
                            } else {
                                serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap()
                            };
                            break (
                                header
                                    .lines()
                                    .next()
                                    .unwrap()
                                    .split_whitespace()
                                    .nth(1)
                                    .unwrap()
                                    .to_owned(),
                                body,
                            );
                        }
                    }
                };
                if path.is_empty() {
                    continue;
                }
                tx.send((path.clone(), body)).await.unwrap();
                let value = if path.contains("tenant_access_token") {
                    json!({"code":0,"tenant_access_token":"mock-token","expire":7200})
                } else if path == "/open-apis/cardkit/v1/cards" {
                    json!({"code":0,"data":{"card_id":"card-1"}})
                } else if path.ends_with("/reply") {
                    json!({"code":0,"data":{"message_id":"reply-1"}})
                } else if path.ends_with("/answer/content") && !failed_once {
                    failed_once = true;
                    json!({"code":99991400,"msg":"temporary failure"})
                } else {
                    json!({"code":0,"data":{}})
                };
                let body = value.to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let home = std::env::temp_dir().join(format!("fae-card-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        let config = serde_json::from_value(json!({"version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant","agent_id":"fae","workspace":home})).unwrap();
        let sdk = Sdk::for_test(config, base);
        let store = Arc::new(Store::open(&home, 7).unwrap());
        let binding = crate::protocol::SessionBinding {
            channel: "lark".into(),
            app_id: "app".into(),
            tenant_key: "tenant".into(),
            user_id: "u".into(),
            chat_id: "chat".into(),
            chat_type: "p2p".into(),
        };
        let id = super::super::identity::receipt_id("app", "tenant", "message");
        let mut record = super::super::store::Record::new(
            id.clone(),
            super::super::identity::session_id(&binding),
            binding,
            "message".into(),
            "q".into(),
            None,
        );
        let mut card = Card::new("response".into());
        card.text = "streamed text".into();
        record.responses.push(card);
        store.insert(record).unwrap();
        let writer = Writer {
            sdk,
            store: store.clone(),
        };
        let first_error = writer.flush(&id).await.unwrap_err();
        let pending = store.get(&id).responses[0]
            .pending
            .clone()
            .unwrap_or_else(|| panic!("Failure occurred before mutation: {first_error:#}"));
        writer.flush(&id).await.unwrap();
        assert!(!store.get(&id).responses[0].closed);
        store
            .update(&id, |r| r.outcome = Some("succeeded".into()))
            .unwrap();
        writer.flush(&id).await.unwrap();
        assert!(store.get(&id).responses[0].closed);
        assert!(store.get(&id).state == State::Delivered);
        let mut observed = Vec::new();
        while let Ok(call) = calls.try_recv() {
            observed.push(call);
        }
        let content: Vec<_> = observed
            .iter()
            .filter(|(p, _)| p.ends_with("/answer/content"))
            .collect();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0].1, content[1].1);
        assert_eq!(content[0].1["uuid"], pending.uuid);
        assert_eq!(
            observed
                .iter()
                .filter(|(p, _)| p.ends_with("/reply"))
                .count(),
            1
        );
        let sequences: Vec<_> = observed
            .iter()
            .filter_map(|(_, b)| b["sequence"].as_u64())
            .collect();
        assert_eq!(sequences, vec![1, 1, 2, 3]);
        server.abort();
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn pages_preserve_unicode_and_serialized_size() {
        let text = "你好\"\\\n".repeat(15000);
        let split = pages(&text);
        assert_eq!(split.concat(), text);
        assert!(
            split
                .iter()
                .all(|p| serde_json::to_vec(p).unwrap().len() <= 24000)
        );
    }
    #[test]
    fn streaming_card_passes_sdk_validation() {
        document().unwrap();
    }
}
