use super::{
    config::Config,
    identity::{receipt_id, session_id},
    store::Record,
};
use crate::protocol::SessionBinding;
use larksuite_oapi_sdk_rs::events::im::P2MessageReceiveV1;

pub fn parse(
    config: &Config,
    bot: &str,
    event: P2MessageReceiveV1,
) -> anyhow::Result<Option<Record>> {
    if event.sender.sender_type != "user" || event.sender.open_id() == Some(bot) {
        return Ok(None);
    }
    if event.sender.tenant_key != config.tenant_key {
        return Ok(None);
    }
    let message = &event.message;
    if !matches!(message.chat_type.as_str(), "p2p" | "group") {
        return Ok(None);
    }
    let mentions_bot = message.mentions.iter().any(|m| m.open_id() == Some(bot));
    if message.chat_type == "group" && !mentions_bot {
        return Ok(None);
    }
    anyhow::ensure!(
        !message.message_id.is_empty() && !message.chat_id.is_empty(),
        "Missing message identity"
    );
    let user = event.sender.open_id().unwrap_or_default();
    let binding = SessionBinding {
        session_mapping_version: 2,
        thread_id: message.thread_id.clone(),
        channel: "lark".into(),
        app_id: config.app_id.clone(),
        tenant_key: config.tenant_key.clone(),
        user_id: user.into(),
        chat_type: message.chat_type.clone(),
        chat_id: message.chat_id.clone(),
    };
    let mut rejection = None;
    let mut text = String::new();
    if user.is_empty() {
        rejection = Some("无法识别飞书用户，消息缺少发送者 open_id".into());
    } else if message.message_type != "text" {
        rejection = Some("目前只支持文本消息".into());
    } else {
        text = serde_json::from_str::<serde_json::Value>(&message.content)?["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        for mention in &message.mentions {
            if mention.key.is_empty() {
                continue;
            }
            text = text.replace(
                &mention.key,
                &if mention.open_id() == Some(bot) {
                    String::new()
                } else {
                    format!("@{}", mention.name)
                },
            );
        }
        text = text.trim().to_owned();
        if text.is_empty() {
            rejection = Some("请输入需要 Agent 处理的文本".into());
        }
    }
    let id = receipt_id(&config.app_id, &config.tenant_key, &message.message_id);
    if !message.thread_id.is_empty() && !super::identity::valid_thread(&message.thread_id) {
        anyhow::bail!("Invalid thread identity");
    }
    let mut record = Record::new(
        id,
        session_id(&binding),
        binding,
        message.message_id.clone(),
        text,
        rejection,
    );
    record.root_id = message.root_id.clone();
    record.parent_id = message.parent_id.clone();
    Ok(Some(record))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn groups_require_real_bot_mention_inside_and_outside_threads() {
        let config: Config = serde_json::from_value(json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant","agent_id":"fae"
        })).unwrap();
        for thread in ["", "omt_T1"] {
            for text in ["hello", "@ou_bot hello", "/clean", "/steer update", "/session new"] {
                for mentioned in [None, Some("ou_other"), Some("ou_bot")] {
                    let event = serde_json::from_value(json!({
                        "sender":{"sender_id":{"open_id":"ou_a"},"sender_type":"user","tenant_key":"tenant"},
                        "message":{"message_id":"om_input","chat_id":"chat","chat_type":"group",
                            "thread_id":thread,"root_id":"om_root","parent_id":"om_parent",
                            "message_type":"text","content":json!({"text":text}).to_string(),
                            "mentions":mentioned.map(|id| vec![json!({"key":"@_user_1","id":{"open_id":id},"name":"Bot"})]).unwrap_or_default()}
                    })).unwrap();
                    let record = parse(&config, "ou_bot", event).unwrap();
                    assert_eq!(record.is_some(), mentioned == Some("ou_bot"));
                    if let Some(record) = record {
                        assert_eq!(record.session_mapping_version, 2);
                        assert_eq!(record.session_id, thread);
                        assert_eq!(record.root_id, "om_root");
                        assert_eq!(record.parent_id, "om_parent");
                    }
                }
            }
        }
    }

    #[test]
    fn agent_identity_uses_only_sender_open_id() {
        let config: Config = serde_json::from_value(json!({
            "version": 1,
            "app_id": "app",
            "app_secret": "secret",
            "tenant_key": "tenant",
            "agent_id": "fae"
        }))
        .unwrap();
        for (sender_id, expected_user, accepted) in [
            (json!({"open_id": "ou_alice"}), "ou_alice", true),
            (
                json!({"open_id": "ou_alice", "user_id": "alice", "union_id": "on_alice"}),
                "ou_alice",
                true,
            ),
            (
                json!({"user_id": "alice", "union_id": "on_alice"}),
                "",
                false,
            ),
            (json!({"open_id": "", "user_id": "alice"}), "", false),
        ] {
            let event = serde_json::from_value(json!({
                "sender": {
                    "sender_id": sender_id,
                    "sender_type": "user",
                    "tenant_key": "tenant"
                },
                "message": {
                    "message_id": "message",
                    "chat_id": "chat",
                    "chat_type": "p2p",
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}"
                }
            }))
            .unwrap();
            let record = parse(&config, "ou_bot", event).unwrap().unwrap();
            assert_eq!(record.binding.user_id, expected_user);
            assert_eq!(record.session_id, session_id(&record.binding));
            assert_eq!(record.rejection.is_none(), accepted);
            if accepted {
                assert_eq!(record.text, "hello");
            } else {
                assert_eq!(
                    record.rejection.as_deref(),
                    Some("无法识别飞书用户，消息缺少发送者 open_id")
                );
            }
        }
    }
}
