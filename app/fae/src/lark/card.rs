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
    #[serde(default)]
    pub final_document: bool,
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
            final_document: false,
            pending: None,
        }
    }
}
fn summary(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(100)
        .collect()
}

pub fn document(text: &str) -> anyhow::Result<Value> {
    let value = json!({
        "schema":"2.0",
        "config":{"streaming_mode":false,"update_multi":true,
            "summary":{"content":summary(text)}},
        "body":{"elements":[
            {"tag":"markdown","content":text,"element_id":"answer"}
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
            "finish" => (
                http::Method::DELETE,
                format!("/open-apis/cardkit/v1/cards/{card_id}/elements/status"),
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
            if matches!(mutation.kind.as_str(), "close" | "finish") {
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
        // Tool preambles and provisional answers stay local until the round completes.
        if record.outcome.is_none() {
            return Ok(());
        }
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
                        .text_record(&self.store, id, &text, &card.reply_uuid)
                        .await?;
                    self.change(id, index, |c| {
                        c.reply_id = Some(reply);
                        c.closed = true;
                    })?;
                }
                continue;
            }
            if card.card_id.is_none() {
                let displayed = display_page(&record.responses, index);
                let value = self
                    .sdk
                    .raw(
                        http::Method::POST,
                        "/open-apis/cardkit/v1/cards",
                        Some(json!({
                            "type":"card_json","data":document(&displayed)?.to_string()
                        })),
                    )
                    .await?;
                let card_id = value["data"]["card_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Card ID missing"))?
                    .to_owned();
                self.change(id, index, |c| {
                    c.card_id = Some(card_id);
                    c.sent_text = displayed;
                    c.final_document = true;
                })?;
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
                    .reply_record(
                        &self.store,
                        id,
                        "interactive",
                        json!({
                            "type":"card","data":{"card_id":card.card_id}
                        })
                        .to_string(),
                        &card.reply_uuid,
                    )
                    .await?;
                self.change(id, index, |c| {
                    c.reply_id = Some(reply);
                    c.closed = c.final_document;
                })?;
            }
            if self.store.get(id).responses[index].closed {
                continue;
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
                } else {
                    ""
                };
                if !status.is_empty() {
                    self.mutation(id, index, "status", json!({"content":status}))
                        .await?;
                }
                let summary = if status.is_empty() {
                    summary(&self.store.get(id).responses[index].text)
                } else {
                    status.to_owned()
                };
                // Structural edits follow streaming shutdown. Successful replies
                // remove the status element rather than submitting empty content.
                let close_kind = if status.is_empty() {
                    "stop_stream"
                } else {
                    "close"
                };
                self.mutation(id, index, close_kind, json!({
                    "settings":json!({"streaming_mode":false,"summary":{"content":summary}}).to_string()
                })).await?;
                if status.is_empty() {
                    self.mutation(id, index, "finish", json!({})).await?;
                }
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
                    .text_record(&self.store, id, notice, &record.notice_uuid)
                    .await?;
                self.store.update(id, |r| r.notice_sent = true)?;
            }
        }
        if record.outcome.is_some()
            && self.store.completion_delivered(&record)
            && record
                .responses
                .iter()
                .all(|c| c.closed || c.text.trim().is_empty())
            && (record.notice.is_none() || self.store.get(id).notice_sent)
        {
            if record.thread_reply_pending_removal && !record.thread_reply_message_id.is_empty() {
                self.sdk.withdraw(&record.thread_reply_message_id).await?;
                self.store
                    .update(id, |r| r.thread_reply_pending_removal = false)?;
            }
            if let Some(reaction) = &record.reaction_id {
                self.sdk.untype(&record.message_id, reaction).await?;
            }
            self.store.update(id, |r| {
                // Typing may complete while delivery cleanup is in flight.
                if r.reaction_id != record.reaction_id {
                    return;
                }
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
    async fn waits_for_finished_then_retries_final_reply_without_placeholder() {
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
                } else if path.ends_with("/reply") && !failed_once {
                    failed_once = true;
                    json!({"code":99991400,"msg":"temporary failure"})
                } else if path.ends_with("/reply") {
                    json!({"code":0,"data":{"message_id":"reply-1","thread_id":"omt_card","chat_id":"chat","root_id":"message"}})
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
            session_mapping_version: 2,
            thread_id: "omt_card".into(),
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
        writer.flush(&id).await.unwrap();
        assert!(calls.try_recv().is_err());
        assert!(store.get(&id).responses[0].card_id.is_none());
        store
            .update(&id, |r| r.outcome = Some("succeeded".into()))
            .unwrap();
        writer.flush(&id).await.unwrap_err();
        let card = store.get(&id).responses[0].clone();
        assert!(card.final_document);
        assert!(card.pending.is_none());
        assert!(!card.closed);
        writer.flush(&id).await.unwrap();
        assert!(store.get(&id).responses[0].closed);
        assert!(store.get(&id).state == State::Delivered);
        let mut observed = Vec::new();
        while let Ok(call) = calls.try_recv() {
            observed.push(call);
        }
        let creations: Vec<_> = observed
            .iter()
            .filter(|(p, _)| p == "/open-apis/cardkit/v1/cards")
            .collect();
        assert_eq!(creations.len(), 1);
        let document: Value =
            serde_json::from_str(creations[0].1["data"].as_str().unwrap()).unwrap();
        assert_eq!(document["config"]["streaming_mode"], false);
        assert_eq!(document["config"]["summary"]["content"], "streamed text");
        assert_eq!(document["body"]["elements"].as_array().unwrap().len(), 1);
        assert_eq!(document["body"]["elements"][0]["content"], "streamed text");
        let replies: Vec<_> = observed
            .iter()
            .filter(|(p, _)| p.ends_with("/reply"))
            .collect();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0].1, replies[1].1);
        assert_eq!(replies[0].1["uuid"], card.reply_uuid);
        assert!(
            observed
                .iter()
                .filter(|(p, _)| p.ends_with("/reply"))
                .all(|(_, b)| b["reply_in_thread"] == true)
        );
        let sequences: Vec<_> = observed
            .iter()
            .filter_map(|(_, b)| b["sequence"].as_u64())
            .collect();
        assert!(sequences.is_empty());
        let restarted = Writer {
            sdk: writer.sdk.clone(),
            store: Arc::new(Store::open(&home, 7).unwrap()),
        };
        restarted.flush(&id).await.unwrap();
        assert!(calls.try_recv().is_err());
        server.abort();
        std::fs::remove_dir_all(home).unwrap();
    }
    #[tokio::test]
    async fn legacy_streaming_card_retries_mutation_and_finishes_without_empty_status() {
        let mut failed = false;
        let (base, requests, task) = super::super::sdk::tests::mock(move |path, body| {
            if path.ends_with("/answer/content") && !failed {
                failed = true;
                json!({"code":99991400,"msg":"temporary failure"})
            } else {
                assert!(!path.ends_with("/status/content"));
                if path.ends_with("/settings") {
                    let settings: Value =
                        serde_json::from_str(body["settings"].as_str().unwrap()).unwrap();
                    assert_eq!(settings["streaming_mode"], false);
                }
                json!({"code":0,"data":{}})
            }
        })
        .await;
        let home = std::env::temp_dir().join(format!("fae-legacy-card-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        let config = serde_json::from_value(json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant",
            "agent_id":"fae","workspace":home
        }))
        .unwrap();
        let store = Arc::new(Store::open(&home, 7).unwrap());
        let binding = serde_json::from_value(json!({
            "session_mapping_version":2,"thread_id":"omt_legacy","channel":"lark",
            "app_id":"app","tenant_key":"tenant","user_id":"u","chat_id":"chat","chat_type":"p2p"
        }))
        .unwrap();
        let id = super::super::identity::receipt_id("app", "tenant", "om_legacy");
        let mut record = super::super::store::Record::new(
            id.clone(),
            "omt_legacy".into(),
            binding,
            "om_legacy".into(),
            "q".into(),
            None,
        );
        let mut card = Card::new("response".into());
        card.text = "最终正文".into();
        card.card_id = Some("legacy".into());
        card.reply_id = Some("om_reply".into());
        let mut saved = serde_json::to_value(card).unwrap();
        saved.as_object_mut().unwrap().remove("final_document");
        record
            .responses
            .push(serde_json::from_value(saved).unwrap());
        record.outcome = Some("succeeded".into());
        store.insert(record).unwrap();
        let writer = Writer {
            sdk: Sdk::for_test(config, base),
            store: store.clone(),
        };
        writer.flush(&id).await.unwrap_err();
        writer.flush(&id).await.unwrap();
        assert!(store.get(&id).responses[0].closed);
        let calls = requests.lock().unwrap();
        let content: Vec<_> = calls
            .iter()
            .filter(|(p, _)| p.ends_with("/answer/content"))
            .collect();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0].1, content[1].1);
        let sequences: Vec<_> = calls
            .iter()
            .filter_map(|(_, b)| b["sequence"].as_u64())
            .collect();
        assert_eq!(sequences, vec![1, 1, 2, 3]);
        drop(calls);
        task.abort();
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
    fn final_card_passes_sdk_validation_and_summary_preserves_unicode() {
        let text = format!("{}\n正文", "玫瑰".repeat(70));
        let card = document(&text).unwrap();
        assert_eq!(card["config"]["summary"]["content"], "玫瑰".repeat(50));
        assert_eq!(card["body"]["elements"][0]["content"], text);
        assert!(card["config"].get("streaming_config").is_none());
    }
}
