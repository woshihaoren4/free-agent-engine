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
    let user = event.sender.user_id().unwrap_or_default();
    let binding = SessionBinding {
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
        rejection = Some("无法识别飞书用户，请管理员检查 user ID 权限".into());
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
    Ok(Some(Record::new(
        id,
        session_id(&binding),
        binding,
        message.message_id.clone(),
        text,
        rejection,
    )))
}
