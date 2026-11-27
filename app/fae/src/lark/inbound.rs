use super::{
    config::Config,
    identity::{receipt_id, session_id},
    store::Record,
};
use crate::session_host::LarkPrincipal as SessionBinding;
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
    let mut images = Vec::new();
    if !config.allows_user(user) {
        rejection = Some("权限不足".into());
    } else if user.is_empty() {
        rejection = Some("无法识别飞书用户，消息缺少发送者 open_id".into());
    } else {
        match serde_json::from_str::<serde_json::Value>(&message.content)
            .map_err(anyhow::Error::from)
            .and_then(|value| super::content::Body::parse(&message.message_type, &value, bot))
        {
            Ok(body) => {
                text = body.text;
                images = body.images;
            }
            Err(error) => rejection = Some(format!("无法读取消息内容：{error}")),
        }
        for mention in &message.mentions {
            let name = if mention.open_id() == Some(bot) {
                String::new()
            } else {
                format!("@{}", mention.name)
            };
            if message.message_type != "text" {
                for id in [
                    Some(mention.key.as_str()),
                    mention.open_id(),
                    mention.user_id(),
                    mention.union_id(),
                ]
                .into_iter()
                .flatten()
                .filter(|id| !id.is_empty())
                {
                    // Cards use inline <at> markup; posts may use either the ID or event placeholder.
                    for markup in [
                        format!("<at id={id}></at>"),
                        format!("<at id=\"{id}\"></at>"),
                        format!("<at user_id=\"{id}\"></at>"),
                        format!("@{id}"),
                    ] {
                        text = text.replace(&markup, &name);
                    }
                }
            }
            if !mention.key.is_empty() {
                text = text.replace(&mention.key, &name);
            }
        }
        text = text.trim().to_owned();
        if rejection.is_none() && text.is_empty() && images.is_empty() {
            rejection = Some("请输入需要 Agent 处理的文字或图片".into());
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
    record.images = images;
    record.root_id = message.root_id.clone();
    record.parent_id = message.parent_id.clone();
    record.message_metadata = super::hook::MessageMetadata {
        bot_open_id: bot.into(),
        sender_type: event.sender.sender_type.clone(),
        sender_user_id: event.sender.user_id().map(str::to_owned),
        sender_union_id: event.sender.union_id().map(str::to_owned),
        message_type: message.message_type.clone(),
        original_root_id: message.root_id.clone(),
        create_time: message.create_time.clone(),
        update_time: message.update_time.clone(),
        mentions: message
            .mentions
            .iter()
            .map(|mention| super::hook::Mention {
                key: mention.key.clone(),
                name: mention.name.clone(),
                open_id: mention.open_id().map(str::to_owned),
                user_id: mention.user_id().map(str::to_owned),
                union_id: mention.union_id().map(str::to_owned),
                mentioned_type: mention.mentioned_type.clone(),
                tenant_key: mention.tenant_key.clone(),
            })
            .collect(),
    };
    Ok(Some(record))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn rich_input_and_image_only_messages_survive_receipt_serialization() {
        let config: Config = serde_json::from_value(json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant","agent_id":"fae"
        }))
        .unwrap();
        for (kind, content, text) in [
            (
                "post",
                json!({"content":[[{"tag":"at","user_id":"ou_bot"},
                {"tag":"text","text":"看图"},{"tag":"img","image_key":"img_one"}]]}),
                "看图",
            ),
            (
                "interactive",
                json!({"body":{"elements":[{"tag":"markdown","content":"看图"},
                {"tag":"img","img_key":"img_one"}]}}),
                "看图",
            ),
            ("image", json!({"image_key":"img_one"}), ""),
        ] {
            let event = serde_json::from_value(json!({
                "sender":{"sender_id":{"open_id":"ou_a"},"sender_type":"user","tenant_key":"tenant"},
                "message":{"message_id":"om_input","chat_id":"chat","chat_type":"group",
                    "message_type":kind,"content":content.to_string(),
                    "mentions":[{"key":"@_user_1","id":{"open_id":"ou_bot"},"name":"Bot"}]}
            })).unwrap();
            let record = parse(&config, "ou_bot", event).unwrap().unwrap();
            assert_eq!(record.text, text);
            assert!(record.rejection.is_none());
            let saved: Record =
                serde_json::from_slice(&serde_json::to_vec(&record).unwrap()).unwrap();
            assert_eq!(
                saved.images,
                vec![super::super::content::Image::Feishu {
                    key: "img_one".into()
                }]
            );
        }
    }

    #[test]
    fn malformed_or_unexpanded_content_gets_an_explicit_rejection() {
        let config: Config = serde_json::from_value(json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant","agent_id":"fae"
        }))
        .unwrap();
        for content in [
            "invalid json",
            "{\"type\":\"template\",\"data\":{\"template_id\":\"id\"}}",
        ] {
            let event = serde_json::from_value(json!({
                "sender":{"sender_id":{"open_id":"ou_a"},"sender_type":"user","tenant_key":"tenant"},
                "message":{"message_id":"om_input","chat_id":"chat","chat_type":"p2p",
                    "message_type":"interactive","content":content}
            })).unwrap();
            assert!(
                parse(&config, "ou_bot", event)
                    .unwrap()
                    .unwrap()
                    .rejection
                    .is_some()
            );
        }
    }
    #[test]
    fn rich_mentions_use_event_names_and_remove_bot_placeholders() {
        let config: Config = serde_json::from_value(json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant","agent_id":"fae"
        }))
        .unwrap();
        for (kind, content) in [
            (
                "post",
                json!({"content":[[
                    {"tag":"at","user_id":"@_user_1"},{"tag":"text","text":"请联系 "},
                    {"tag":"at","user_id":"ou_alice"}
                ]]}),
            ),
            (
                "interactive",
                json!({"elements":[{"tag":"markdown",
                "content":"<at id=ou_bot></at>请联系 <at id=\"ou_alice\"></at>"}]}),
            ),
        ] {
            let event = serde_json::from_value(json!({
                "sender":{"sender_id":{"open_id":"ou_user"},"sender_type":"user","tenant_key":"tenant"},
                "message":{"message_id":"om_input","chat_id":"chat","chat_type":"group",
                    "message_type":kind,"content":content.to_string(),"mentions":[
                        {"key":"@_user_1","id":{"open_id":"ou_bot"},"name":"Bot"},
                        {"key":"@_user_2","id":{"open_id":"ou_alice"},"name":"Alice"}
                    ]}
            })).unwrap();
            let record = parse(&config, "ou_bot", event).unwrap().unwrap();
            assert_eq!(record.text, "请联系 @Alice");
        }
    }
    #[test]
    fn retains_event_metadata_separately_from_normalized_query() {
        let config: Config = serde_json::from_value(json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant","agent_id":"fae"
        }))
        .unwrap();
        let event = serde_json::from_value(json!({
            "sender":{"sender_id":{"open_id":"ou_alice","user_id":"alice","union_id":"on_alice"},
                "sender_type":"user","tenant_key":"tenant"},
            "message":{"message_id":"om_input","chat_id":"oc_chat","chat_type":"group",
                "thread_id":"omt_T1","root_id":"om_root","parent_id":"om_parent",
                "create_time":"1234567890000","update_time":"1234567890001",
                "message_type":"text","content":json!({"text":"@_user_1 请联系 @_user_2"}).to_string(),
                "mentions":[
                    {"key":"@_user_1","id":{"open_id":"ou_bot"},"name":"Bot","mentioned_type":"bot"},
                    {"key":"@_user_2","id":{"open_id":"ou_zhang"},"name":"张三","mentioned_type":"user","tenant_key":"tenant"}
                ]}
        })).unwrap();
        let record = parse(&config, "ou_bot", event).unwrap().unwrap();
        assert_eq!(record.text, "请联系 @张三");
        assert_eq!(record.message_metadata.bot_open_id, "ou_bot");
        assert_eq!(
            record.message_metadata.sender_user_id.as_deref(),
            Some("alice")
        );
        assert_eq!(
            record.message_metadata.sender_union_id.as_deref(),
            Some("on_alice")
        );
        assert_eq!(record.message_metadata.create_time, "1234567890000");
        assert_eq!(record.message_metadata.original_root_id, "om_root");
        assert_eq!(record.message_metadata.update_time, "1234567890001");
        assert_eq!(record.message_metadata.mentions.len(), 2);
        assert_eq!(
            record.message_metadata.mentions[1].open_id.as_deref(),
            Some("ou_zhang")
        );
    }
    #[test]
    fn whitelist_filters_all_interactions_by_sender_open_id() {
        for (whitelist, allowed) in [
            (None, true),
            (Some(json!(null)), true),
            (Some(json!([])), false),
            (Some(json!(["ou_alice", "ou_bob"])), true),
            (Some(json!(["ou_bob"])), false),
            // A sender's employee user_id cannot substitute for their open_id.
            (Some(json!(["alice"])), false),
        ] {
            let mut value = json!({
                "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant","agent_id":"fae"
            });
            if let Some(whitelist) = whitelist {
                value["user_id_whitelist"] = whitelist;
            }
            let config: Config = serde_json::from_value(value).unwrap();
            for chat_type in ["p2p", "group"] {
                for thread in ["", "omt_T1"] {
                    for text in ["hello", "/help", "/clean", "/steer update", "/todo task"] {
                        let event = serde_json::from_value(json!({
                            "sender":{"sender_id":{"open_id":"ou_alice","user_id":"alice"},
                                "sender_type":"user","tenant_key":"tenant"},
                            "message":{"message_id":"om_input","chat_id":"chat","chat_type":chat_type,
                                "thread_id":thread,"message_type":"text",
                                "content":json!({"text":text}).to_string(),
                                "mentions":[{"key":"@_user_1","id":{"open_id":"ou_bot"},"name":"Bot"}]}
                        })).unwrap();
                        let record = parse(&config, "ou_bot", event).unwrap().unwrap();
                        if allowed {
                            assert!(record.rejection.is_none());
                            assert_eq!(record.text, text);
                        } else {
                            assert_eq!(record.rejection.as_deref(), Some("权限不足"));
                            assert!(record.text.is_empty());
                        }
                    }
                }
            }
            if !allowed {
                // Denied users receive the permission notice even when their
                // message content cannot be parsed.
                let event = serde_json::from_value(json!({
                    "sender":{"sender_id":{"open_id":"ou_alice"},"sender_type":"user","tenant_key":"tenant"},
                    "message":{"message_id":"om_bad","chat_id":"chat","chat_type":"p2p",
                        "message_type":"text","content":"invalid json"}
                })).unwrap();
                let record = parse(&config, "ou_bot", event).unwrap().unwrap();
                assert_eq!(record.rejection.as_deref(), Some("权限不足"));
            }
        }
    }

    #[test]
    fn groups_require_real_bot_mention_inside_and_outside_threads() {
        let config: Config = serde_json::from_value(json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant","agent_id":"fae"
        }))
        .unwrap();
        for thread in ["", "omt_T1"] {
            for text in [
                "hello",
                "@ou_bot hello",
                "/clean",
                "/steer update",
                "/session new",
            ] {
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
