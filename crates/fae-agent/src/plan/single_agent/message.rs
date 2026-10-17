use super::*;

pub(super) fn estimated_tokens(request: &CreateChatCompletionRequest) -> usize {
    estimated_tokens_with_image_cost(request, image_token_cost())
}

pub(super) fn estimated_tokens_with_image_cost(
    request: &CreateChatCompletionRequest,
    image_token_cost: usize,
) -> usize {
    let mut request = request.clone();
    let image_count = request
        .messages
        .iter_mut()
        .filter_map(|message| match message {
            ChatCompletionRequestMessage::User(message) => match &mut message.content {
                ChatCompletionRequestUserMessageContent::Array(parts) => Some(parts),
                ChatCompletionRequestUserMessageContent::Text(_) => None,
            },
            _ => None,
        })
        .map(|parts| {
            let original_len = parts.len();
            parts.retain(|part| {
                !matches!(
                    part,
                    ChatCompletionRequestUserMessageContentPart::ImageUrl(_)
                )
            });
            original_len - parts.len()
        })
        .sum::<usize>();

    let text_tokens = serde_json::to_string(&request)
        .map(|json| json.chars().count().div_ceil(4).max(1))
        .unwrap_or(1);
    text_tokens.saturating_add(image_count.saturating_mul(image_token_cost))
}

pub(super) fn image_token_cost() -> usize {
    image_token_cost_from(std::env::var(IMAGE_TOKEN_COST_ENV).ok().as_deref())
}

pub(super) fn image_token_cost_from(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_IMAGE_TOKEN_COST)
}

pub(super) fn summary_chat_message(content: &str) -> ChatCompletionRequestMessage {
    ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
        content: format!("Compressed conversation context:\n{content}").into(),
        ..Default::default()
    })
}

pub(super) async fn session_message_to_chat(
    message: &SessionMessage,
    image_detail: &ImageDetail,
) -> anyhow::Result<ChatCompletionRequestMessage> {
    Ok(match &message.role {
        SessionMessageRole::User => {
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: user_message_content(
                    &message.content,
                    message.images.as_deref().unwrap_or_default(),
                    image_detail,
                )
                .await?,
                ..Default::default()
            })
        }
        SessionMessageRole::Assistant => {
            ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
                content: Some(message.content.clone().into()),
                ..Default::default()
            })
        }
        SessionMessageRole::Summary => summary_chat_message(&message.content),
    })
}

pub(super) async fn user_message_content(
    text: &str,
    images: &[String],
    image_detail: &ImageDetail,
) -> anyhow::Result<ChatCompletionRequestUserMessageContent> {
    if images.is_empty() {
        return Ok(ChatCompletionRequestUserMessageContent::Text(
            text.to_string(),
        ));
    }

    let mut parts = Vec::with_capacity(images.len() + usize::from(!text.is_empty()));
    if !text.is_empty() {
        parts.push(ChatCompletionRequestUserMessageContentPart::Text(
            ChatCompletionRequestMessageContentPartText {
                text: text.to_string(),
            },
        ));
    }
    for image in images {
        let url = resolve_image_url(image).await?;
        parts.push(ChatCompletionRequestUserMessageContentPart::ImageUrl(
            ChatCompletionRequestMessageContentPartImage {
                image_url: ImageUrl {
                    url,
                    detail: Some(image_detail.clone()),
                },
            },
        ));
    }
    Ok(ChatCompletionRequestUserMessageContent::Array(parts))
}

async fn resolve_image_url(image: &str) -> anyhow::Result<String> {
    if image.starts_with("http://")
        || image.starts_with("https://")
        || image.starts_with("data:image/")
    {
        return Ok(image.to_string());
    }

    let path = Path::new(image);
    let format = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .and_then(|extension| match extension.as_str() {
            "jpg" | "jpeg" => Some("jpeg"),
            "png" => Some("png"),
            "gif" => Some("gif"),
            "webp" => Some("webp"),
            _ => None,
        })
        .with_context(|| format!("unsupported image format for `{}`", path.display()))?;
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read attached image `{}`", path.display()))?;
    Ok(format!(
        "data:image/{format};base64,{}",
        bytes.base64_encode_std()
    ))
}
