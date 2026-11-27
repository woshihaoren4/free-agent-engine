use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Resolve images before submission; inline data travels with the accepted request and survives restart.
pub async fn input(
    sdk: &super::sdk::Sdk,
    record: &super::store::Record,
    text: String,
) -> anyhow::Result<fae_agent::protocol::Content> {
    use fae_agent::protocol::{ContentBlock, ResourceRef};
    // Leave room for base64 expansion and the envelope in the service's 8 MiB frame.
    const IMAGE_URI_BYTES: usize = 6 * 1024 * 1024;
    let mut budget = IMAGE_URI_BYTES.saturating_sub(text.len());
    let mut content = crate::app::client::text_content(text);
    for image in &record.images {
        let resource = match image {
            Image::Feishu { key } => {
                let resource = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    sdk.image(
                        &record.message_id,
                        key,
                        (budget / 4 * 3).saturating_sub(128).min(4 * 1024 * 1024),
                    ),
                )
                .await
                .map_err(|_| anyhow::anyhow!("下载飞书图片超时"))??;
                anyhow::ensure!(resource.uri.len() <= budget, "消息图片总大小超过限制");
                budget -= resource.uri.len();
                resource
            }
            Image::Url { url } => {
                anyhow::ensure!(
                    url.starts_with("https://")
                        || url.starts_with("http://")
                        || url.starts_with("data:image/"),
                    "不支持的图片地址"
                );
                anyhow::ensure!(url.len() <= budget, "消息图片总大小超过限制");
                budget -= url.len();
                ResourceRef {
                    uri: url.clone(),
                    media_type: "image/*".into(),
                    name: None,
                }
            }
        };
        content.push(ContentBlock::Resource { resource });
    }
    Ok(content)
}

/// Durable image locations; Feishu keys must be resolved with the receiving bot's credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Image {
    Feishu { key: String },
    Url { url: String },
}

#[derive(Default, Debug)]
pub struct Body {
    pub text: String,
    pub images: Vec<Image>,
}

impl Body {
    pub fn parse(kind: &str, value: &Value, bot: &str) -> anyhow::Result<Self> {
        let mut body = Self::default();
        match kind {
            "text" => body.markdown(value["text"].as_str().unwrap_or_default()),
            "image" => body.image(value),
            "post" => {
                // Receive events normally contain the unwrapped post; also accept localized bodies.
                let post = if value.get("content").is_some() {
                    value
                } else {
                    value
                        .get("zh_cn")
                        .or_else(|| value.get("en_us"))
                        .or_else(|| {
                            value
                                .as_object()
                                .and_then(|v| v.values().find(|v| v.get("content").is_some()))
                        })
                        .unwrap_or(value)
                };
                body.visible(&post["title"], bot, 0);
                body.newline();
                if let Some(rows) = post["content"].as_array() {
                    for row in rows {
                        if let Some(nodes) = row.as_array() {
                            for node in nodes {
                                body.node(node, bot, 0);
                            }
                        } else {
                            body.node(row, bot, 0);
                        }
                        body.newline();
                    }
                }
            }
            "interactive" => body.node(value, bot, 0),
            _ => anyhow::bail!("目前支持文本、富文本、卡片和图片消息"),
        }
        body.text = body.text.trim().to_owned();
        anyhow::ensure!(body.images.len() <= 20, "单条消息最多支持 20 张图片");
        Ok(body)
    }

    fn newline(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
    }

    fn add_image(&mut self, image: Image) {
        if !self.images.contains(&image) {
            self.images.push(image);
        }
    }

    fn image_url(&mut self, url: &str) -> bool {
        if url.starts_with("https://")
            || url.starts_with("http://")
            || url.starts_with("data:image/")
        {
            self.add_image(Image::Url { url: url.into() });
            true
        } else if url.starts_with("img_") {
            self.add_image(Image::Feishu { key: url.into() });
            true
        } else {
            false
        }
    }

    fn image(&mut self, node: &Value) {
        if let Some(key) = node["image_key"]
            .as_str()
            .or_else(|| node["img_key"].as_str())
            .filter(|key| !key.is_empty())
        {
            self.add_image(Image::Feishu { key: key.into() });
        } else if let Some(url) = node["url"].as_str().or_else(|| node["src"].as_str()) {
            self.image_url(url);
        }
    }

    /// Keep Markdown intact except image markup, whose alt text and resource are separated.
    fn markdown(&mut self, text: &str) {
        let mut replacements = Vec::new();
        let mut current: Option<(usize, String)> = None;
        for (event, range) in Parser::new(text).into_offset_iter() {
            match event {
                Event::Start(Tag::Image { dest_url, .. }) if self.image_url(&dest_url) => {
                    current = Some((range.start, String::new()));
                }
                Event::Text(value) | Event::Code(value) => {
                    if let Some((_, alt)) = &mut current {
                        alt.push_str(&value);
                    }
                }
                Event::End(TagEnd::Image) => {
                    if let Some((start, alt)) = current.take() {
                        replacements.push((start..range.end, alt));
                    }
                }
                _ => {}
            }
        }
        let mut offset = 0;
        for (range, alt) in replacements {
            self.text.push_str(&text[offset..range.start]);
            self.text.push_str(&alt);
            offset = range.end;
        }
        self.text.push_str(&text[offset..]);
    }

    fn visible(&mut self, value: &Value, bot: &str, depth: usize) {
        if let Some(text) = value.as_str() {
            self.markdown(text);
        } else {
            self.node(value, bot, depth + 1);
        }
    }

    /// Traverse display fields only: action values, configs, IDs and template data are not text.
    fn node(&mut self, node: &Value, bot: &str, depth: usize) {
        if depth > 64 {
            return;
        }
        if let Some(nodes) = node.as_array() {
            for node in nodes {
                self.node(node, bot, depth + 1);
                self.newline();
            }
            return;
        }
        if !node.is_object() {
            return;
        }
        let tag = node["tag"].as_str().unwrap_or_default();
        match tag {
            "img" | "image" => {
                self.image(node);
                self.visible(&node["alt"], bot, depth);
                self.visible(&node["title"], bot, depth);
                return;
            }
            "at" => {
                let user = node["user_id"].as_str().unwrap_or_default();
                if user != bot {
                    let name = node["user_name"]
                        .as_str()
                        .or_else(|| node["name"].as_str())
                        .unwrap_or(user);
                    self.text.push('@');
                    self.text
                        .push_str(if name == "all" { "所有人" } else { name });
                }
                return;
            }
            "a" => {
                self.visible(&node["text"], bot, depth);
                if let Some(url) = node["href"].as_str() {
                    self.text.push_str(&format!(" ({url})"));
                }
                return;
            }
            "text" | "plain_text" | "lark_md" | "markdown" | "md" => {
                self.visible(
                    node.get("text")
                        .or_else(|| node.get("content"))
                        .unwrap_or(&Value::Null),
                    bot,
                    depth,
                );
                return;
            }
            "code_block" => {
                self.text.push_str("```");
                self.text
                    .push_str(node["language"].as_str().unwrap_or_default());
                self.text.push('\n');
                self.text.push_str(
                    node["text"]
                        .as_str()
                        .or_else(|| node["content"].as_str())
                        .unwrap_or_default(),
                );
                self.text.push_str("\n```\n");
                return;
            }
            "hr" => {
                self.newline();
                return;
            }
            "emotion" => {
                if let Some(emoji) = node["emoji_type"].as_str() {
                    self.text.push_str(&format!(":{emoji}:"));
                }
                return;
            }
            "table" => {
                if let Some(columns) = node["columns"].as_array() {
                    for (index, column) in columns.iter().enumerate() {
                        if index > 0 {
                            self.text.push('\t');
                        }
                        self.visible(&column["display_name"], bot, depth);
                    }
                    self.newline();
                    if let Some(rows) = node["rows"].as_array() {
                        for row in rows {
                            for (index, column) in columns.iter().enumerate() {
                                if index > 0 {
                                    self.text.push('\t');
                                }
                                if let Some(name) = column["name"].as_str() {
                                    let cell = &row[name];
                                    if cell.is_number() || cell.is_boolean() {
                                        self.text.push_str(&cell.to_string());
                                    } else {
                                        self.visible(cell, bot, depth);
                                    }
                                }
                            }
                            self.newline();
                        }
                    }
                    return;
                }
            }
            _ => {}
        }
        // Both card JSON 1.0/2.0 and message API's simplified card representation.
        for field in [
            "header",
            "title",
            "subtitle",
            "text",
            "content",
            "label",
            "placeholder",
            "alt",
            "summary",
        ] {
            if let Some(value) = node.get(field) {
                self.visible(value, bot, depth);
                self.newline();
            }
        }
        if tag == "button" {
            if let Some(url) = node["url"]
                .as_str()
                .or_else(|| node["multi_url"]["url"].as_str())
            {
                self.text.push_str(url);
                self.newline();
            }
        }
        for field in [
            "body", "elements", "columns", "column", "fields", "actions", "options", "rows",
            "cells", "items", "img_list",
        ] {
            if let Some(value) = node.get(field) {
                self.node(value, bot, depth + 1);
                self.newline();
            }
        }
        if node.get("img_key").is_some() || node.get("image_key").is_some() {
            self.image(node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn post_keeps_rows_mentions_links_and_images_without_duplicate_locales() {
        let post = json!({"title":"报告","content":[
            [{"tag":"at","user_id":"ou_bot"},{"tag":"text","text":"请看 "},
             {"tag":"a","text":"文档","href":"https://example.com/doc"}],
            [{"tag":"img","image_key":"img_one"},{"tag":"text","text":"说明"},
             {"tag":"at","user_id":"ou_alice","user_name":"Alice"}]
        ]});
        for value in [
            post.clone(),
            json!({"zh_cn":post,"en_us":{"title":"ignore","content":[]}}),
        ] {
            let body = Body::parse("post", &value, "ou_bot").unwrap();
            assert_eq!(
                body.text,
                "报告\n请看 文档 (https://example.com/doc)\n说明@Alice"
            );
            assert_eq!(
                body.images,
                vec![Image::Feishu {
                    key: "img_one".into()
                }]
            );
        }
    }

    #[test]
    fn cards_extract_visible_fields_from_nested_layouts_only() {
        let elements = json!([
            {"tag":"div","text":{"tag":"lark_md","content":"**详情**"}},
            {"tag":"column_set","columns":[{"tag":"column","elements":[
                {"tag":"img","img_key":"img_one","alt":{"tag":"plain_text","content":"截图"}},
                {"tag":"button","text":{"tag":"plain_text","content":"查看"},"url":"https://example.com",
                 "value":{"text":"private callback data"}}
            ]}]},
            {"tag":"note","elements":[{"tag":"plain_text","content":"备注"}]},
            {"tag":"markdown","content":"图片 ![示例](https://example.com/p.png)"},
            {"tag":"unknown","text":"未来组件","config":{"text":"hidden"}}
        ]);
        for value in [
            json!({"header":{"title":{"tag":"plain_text","content":"标题"}},"elements":elements}),
            json!({"schema":"2.0","header":{"title":{"tag":"plain_text","content":"标题"}},
                "body":{"elements":elements},"config":{"summary":{"content":"hidden"}}}),
        ] {
            let body = Body::parse("interactive", &value, "ou_bot").unwrap();
            assert_eq!(
                body.text,
                "标题\n**详情**\n截图\n查看\nhttps://example.com\n备注\n图片 示例\n未来组件"
            );
            assert_eq!(
                body.images,
                vec![
                    Image::Feishu {
                        key: "img_one".into()
                    },
                    Image::Url {
                        url: "https://example.com/p.png".into()
                    }
                ]
            );
        }
    }

    #[test]
    fn markdown_images_keep_alt_text_but_code_is_not_an_attachment() {
        let body = Body::parse("text", &json!({"text":
            "前 ![一](https://example.com/1.png) 中 ![二][pic]\n`![code](https://example.com/code.png)`\n\n[pic]: https://example.com/2.png"
        }), "").unwrap();
        assert!(body.text.starts_with("前 一 中 二\n`![code]"));
        assert_eq!(body.images.len(), 2);
    }

    #[test]
    fn image_only_and_simplified_card_bodies_are_supported() {
        let body = Body::parse("image", &json!({"image_key":"img_one"}), "").unwrap();
        assert!(body.text.is_empty());
        assert_eq!(body.images.len(), 1);
        let body = Body::parse(
            "interactive",
            &json!({"title":"卡片","elements":[
            {"tag":"text","text":"正文"},{"tag":"img","image_key":"img_one"},
            {"tag":"img","image_key":"img_one"}]}),
            "",
        )
        .unwrap();
        assert_eq!(body.text, "卡片\n正文");
        assert_eq!(body.images.len(), 1);
        assert!(Body::parse("file", &json!({}), "").is_err());
    }

    #[test]
    fn card_table_keeps_column_names_and_typed_cells() {
        let body = Body::parse("interactive", &json!({"elements":[{"tag":"table",
            "columns":[{"name":"name","display_name":"名称"},{"name":"count","display_name":"数量"},
                {"name":"image","display_name":"图片"}],
            "rows":[{"name":"样例","count":3,"image":{"tag":"img","img_key":"img_one"},"hidden":"ignore"}]
        }]}), "").unwrap();
        assert_eq!(body.text, "名称\t数量\t图片\n样例\t3");
        assert_eq!(body.images.len(), 1);
    }

    #[tokio::test]
    async fn extracted_images_become_agent_image_input_and_survive_request_serialization() {
        let (base, requests, task) = super::super::sdk::tests::mock_bytes(|path, _| {
            assert!(path.ends_with("/om_input/resources/img_one?type=image"));
            b"\x89PNG\r\n\x1a\nimage".to_vec()
        })
        .await;
        let config = serde_json::from_value(json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant","agent_id":"fae"
        }))
        .unwrap();
        let sdk = super::super::sdk::Sdk::for_test(config, base);
        let binding = serde_json::from_value(json!({
            "channel":"lark","app_id":"app","tenant_key":"tenant",
            "user_id":"ou_a","chat_type":"p2p","chat_id":"chat"
        }))
        .unwrap();
        let mut record = super::super::store::Record::new(
            "receipt".into(),
            "session".into(),
            binding,
            "om_input".into(),
            "看图".into(),
            None,
        );
        record.images = vec![
            Image::Feishu {
                key: "img_one".into(),
            },
            Image::Url {
                url: "https://example.com/2.png".into(),
            },
        ];
        let content = input(&sdk, &record, record.text.clone()).await.unwrap();
        let content = serde_json::from_slice(&serde_json::to_vec(&content).unwrap()).unwrap();
        let agent = crate::server::session::input_data(&content).await.unwrap();
        assert_eq!(agent.text, "看图");
        let images = agent.images.unwrap();
        assert_eq!(images.len(), 2);
        assert!(images[0].starts_with("data:image/png;base64,"));
        assert_eq!(images[1], "https://example.com/2.png");
        let content = input(&sdk, &record, String::new()).await.unwrap();
        let agent = crate::server::session::input_data(&content).await.unwrap();
        assert!(agent.text.is_empty());
        assert_eq!(agent.images.unwrap().len(), 2);
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.contains("/resources/"))
                .count(),
            2
        );
        task.abort();
    }
}
