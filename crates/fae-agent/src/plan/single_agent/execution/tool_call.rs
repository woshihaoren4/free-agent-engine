use super::*;

#[derive(Debug, Default)]
pub(super) struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCallAccumulator {
    pub(super) fn merge(&mut self, chunk: ChatCompletionMessageToolCallChunk) {
        if let Some(id) = chunk.id {
            self.id.push_str(&id);
        }
        if let Some(function) = chunk.function {
            if let Some(name) = function.name {
                self.name.push_str(&name);
            }
            if let Some(arguments) = function.arguments {
                self.arguments.push_str(&arguments);
            }
        }
    }

    pub(super) fn finish(self) -> anyhow::Result<ChatCompletionMessageToolCalls> {
        anyhow::ensure!(!self.id.is_empty(), "streamed tool call is missing id");
        anyhow::ensure!(!self.name.is_empty(), "streamed tool call is missing name");
        Ok(ChatCompletionMessageToolCalls::Function(
            ChatCompletionMessageToolCall {
                id: self.id,
                function: FunctionCall {
                    name: self.name,
                    arguments: self.arguments,
                },
            },
        ))
    }
}
