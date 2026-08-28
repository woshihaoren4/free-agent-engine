use std::fmt::Debug;

use async_openai::types::chat::{ChatCompletionResponseStream, CreateChatCompletionResponse};

pub enum ModelResponse {
    Completed(CreateChatCompletionResponse),
    Streaming(ChatCompletionResponseStream),
}

impl Debug for ModelResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed(response) => f.debug_tuple("Completed").field(response).finish(),
            Self::Streaming(_) => f.write_str("Streaming(<chat completion stream>)"),
        }
    }
}

impl ModelResponse {
    pub fn is_streaming(&self) -> bool {
        matches!(self, Self::Streaming(_))
    }

    pub fn into_completed(self) -> Option<CreateChatCompletionResponse> {
        match self {
            Self::Completed(response) => Some(response),
            Self::Streaming(_) => None,
        }
    }

    pub fn into_stream(self) -> Option<ChatCompletionResponseStream> {
        match self {
            Self::Completed(_) => None,
            Self::Streaming(stream) => Some(stream),
        }
    }
}
