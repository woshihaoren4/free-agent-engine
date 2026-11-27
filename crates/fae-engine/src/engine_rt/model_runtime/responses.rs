//! Adapt the engine's model contract to the Responses transport.
use std::{
    pin::Pin,
    task::{Context, Poll},
};

use async_openai::{
    error::OpenAIError,
    types::{
        chat::{
            ChatCompletionResponseStream, CreateChatCompletionRequest,
            CreateChatCompletionResponse, CreateChatCompletionStreamResponse, FinishReason,
        },
        responses::{
            CreateResponse, OutputItem, OutputMessageContent, Response, ResponseStream,
            ResponseStreamEvent, Status,
        },
    },
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio_stream::Stream;

fn invalid(message: impl Into<String>) -> OpenAIError {
    OpenAIError::InvalidArgument(message.into())
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, OpenAIError> {
    serde_json::from_value(value)
        .map_err(|error| invalid(format!("Responses API conversion failed: {error}")))
}

fn encode(value: impl Serialize) -> Result<Value, OpenAIError> {
    serde_json::to_value(value).map_err(|error| invalid(error.to_string()))
}

pub(super) fn request(req: CreateChatCompletionRequest) -> Result<CreateResponse, OpenAIError> {
    let mut source = encode(req)?.as_object().cloned().unwrap_or_default();
    let mut target = serde_json::Map::new();
    let messages = source.remove("messages").unwrap_or_default();
    let mut input = Vec::new();
    for message in messages.as_array().into_iter().flatten() {
        let role = message["role"].as_str().unwrap_or_default();
        if role == "tool" {
            input.push(json!({
                "type": "function_call_output",
                "call_id": message["tool_call_id"],
                "output": content(message["content"].clone())?,
            }));
            continue;
        }
        if !matches!(role, "system" | "developer" | "user" | "assistant") {
            return Err(invalid(format!(
                "Responses API does not support message role {role}"
            )));
        }
        for field in ["audio", "function_call", "name"] {
            if !message[field].is_null() {
                return Err(invalid(format!(
                    "Responses API does not support message.{field}"
                )));
            }
        }
        if !message["content"].is_null() {
            input.push(json!({
                "role": role,
                "content": content(message["content"].clone())?,
            }));
        }
        if let Some(refusal) = message["refusal"].as_str() {
            input.push(json!({"role": role, "content": refusal}));
        }
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            if call["type"] != "function" {
                return Err(invalid(
                    "Responses adapter only supports function tool calls",
                ));
            }
            input.push(json!({
                "type": "function_call", "call_id": call["id"],
                "name": call["function"]["name"], "arguments": call["function"]["arguments"],
            }));
        }
    }
    let input = input
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            decode::<async_openai::types::responses::InputItem>(item)
                .map_err(|error| invalid(format!("input item {index}: {error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    target.insert("input".into(), encode(input)?);
    for field in [
        "model",
        "stream",
        "store",
        "temperature",
        "top_p",
        "parallel_tool_calls",
        "metadata",
        "service_tier",
        "safety_identifier",
        "prompt_cache_key",
        "top_logprobs",
    ] {
        if let Some(value) = source.remove(field) {
            target.insert(field.into(), value);
        }
    }
    // The engine sends full conversation history on every request.
    target.entry("store").or_insert(json!(false));
    let legacy_max = source.remove("max_tokens");
    if let Some(max) = source.remove("max_completion_tokens").or(legacy_max) {
        target.insert("max_output_tokens".into(), max);
    }
    if let Some(effort) = source.remove("reasoning_effort") {
        target.insert("reasoning".into(), json!({"effort": effort}));
    }
    let mut text = serde_json::Map::new();
    if let Some(verbosity) = source.remove("verbosity") {
        text.insert("verbosity".into(), verbosity);
    }
    if let Some(mut format) = source.remove("response_format") {
        if format["type"] == "json_schema" {
            let schema = format["json_schema"].take();
            format = schema;
            format["type"] = json!("json_schema");
        }
        text.insert("format".into(), format);
    }
    if !text.is_empty() {
        target.insert("text".into(), Value::Object(text));
    }
    if let Some(tools) = source.remove("tools") {
        let tools = tools
            .as_array()
            .ok_or_else(|| invalid("tools must be an array"))?;
        let tools = tools
            .iter()
            .map(|tool| {
                if tool["type"] != "function" {
                    return Err(invalid("Responses adapter only supports function tools"));
                }
                let mut function = tool["function"].clone();
                function["type"] = json!("function");
                // Chat defaults to non-strict schemas; Responses defaults to strict schemas.
                if function.get("strict").is_none() || function["strict"].is_null() {
                    function["strict"] = json!(false);
                }
                Ok(function)
            })
            .collect::<Result<Vec<_>, _>>()?;
        target.insert("tools".into(), json!(tools));
    }
    if let Some(mut choice) = source.remove("tool_choice") {
        if choice.is_object() && choice["type"] == "function" {
            choice = json!({"type": "function", "name": choice["function"]["name"]});
        }
        target.insert("tool_choice".into(), choice);
    }
    if let Some(options) = source.remove("stream_options") {
        if let Some(obfuscation) = options.get("include_obfuscation") {
            target.insert(
                "stream_options".into(),
                json!({"include_obfuscation": obfuscation}),
            );
        }
        // Responses terminal events always contain usage.
    }
    if let Some(logprobs) = source.remove("logprobs") {
        if logprobs == true {
            target.insert("include".into(), json!(["message.output_text.logprobs"]));
        }
    }
    for (field, default) in [
        ("n", json!(1)),
        ("frequency_penalty", json!(0.0)),
        ("presence_penalty", json!(0.0)),
        ("modalities", json!(["text"])),
    ] {
        if let Some(value) = source.remove(field) {
            if value != default {
                return Err(invalid(format!(
                    "Responses API does not support {field}={value}"
                )));
            }
        }
    }
    if !source.is_empty() {
        return Err(invalid(format!(
            "Responses API does not support chat parameters: {}",
            source.keys().cloned().collect::<Vec<_>>().join(", ")
        )));
    }
    decode(Value::Object(target))
}

fn content(value: Value) -> Result<Value, OpenAIError> {
    if value.is_string() {
        return Ok(value);
    }
    let parts = value
        .as_array()
        .ok_or_else(|| invalid("invalid message content"))?;
    let parts = parts
        .iter()
        .map(|part| match part["type"].as_str() {
            Some("text") => Ok(json!({"type": "input_text", "text": part["text"]})),
            Some("image_url") => Ok(json!({
                "type": "input_image", "image_url": part["image_url"]["url"],
            "detail": part["image_url"].get("detail").filter(|detail| !detail.is_null())
                .cloned().unwrap_or(json!("auto")),
            })),
            Some("file") => {
                let mut file = part["file"].clone();
                file["type"] = json!("input_file");
                Ok(file)
            }
            _ => Err(invalid(
                "Responses API does not support this message content part",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::Array(parts))
}

fn finish_reason(response: &Response) -> Result<FinishReason, OpenAIError> {
    if let Some(error) = &response.error {
        return Err(invalid(format!("Responses API failed: {error:?}")));
    }
    match response.status {
        Status::Completed => Ok(
            if response
                .output
                .iter()
                .any(|item| matches!(item, OutputItem::FunctionCall(_)))
            {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            },
        ),
        Status::Incomplete => match response
            .incomplete_details
            .as_ref()
            .map(|details| details.reason.as_str())
        {
            Some("max_output_tokens") => Ok(FinishReason::Length),
            Some("content_filter") => Ok(FinishReason::ContentFilter),
            reason => Err(invalid(format!("Responses API incomplete: {reason:?}"))),
        },
        ref status => Err(invalid(format!(
            "Responses API did not complete: {status:?}"
        ))),
    }
}

fn usage(response: &Response) -> Result<Value, OpenAIError> {
    let Some(usage) = &response.usage else {
        return Ok(Value::Null);
    };
    let usage = encode(usage)?;
    Ok(json!({
        "prompt_tokens": usage["input_tokens"], "completion_tokens": usage["output_tokens"],
        "total_tokens": usage["total_tokens"],
        "prompt_tokens_details": usage["input_tokens_details"],
        "completion_tokens_details": usage["output_tokens_details"],
    }))
}

pub(super) fn completed(response: Response) -> Result<CreateChatCompletionResponse, OpenAIError> {
    let finish = finish_reason(&response)?;
    let mut text = String::new();
    let mut refusal = String::new();
    let mut calls = Vec::new();
    for item in &response.output {
        match item {
            OutputItem::Message(message) => {
                for part in &message.content {
                    match part {
                        OutputMessageContent::OutputText(part) => text.push_str(&part.text),
                        OutputMessageContent::Refusal(part) => refusal.push_str(&part.refusal),
                    }
                }
            }
            OutputItem::FunctionCall(call) => calls.push(json!({
                "id": call.call_id, "type": "function",
                "function": {"name": call.name, "arguments": call.arguments},
            })),
            OutputItem::Reasoning(_) => {}
            _ => return Err(invalid("unsupported Responses output item")),
        }
    }
    decode(json!({
        "id": response.id, "created": response.created_at, "model": response.model,
        "object": "chat.completion", "service_tier": response.service_tier,
        "usage": usage(&response)?,
        "choices": [{"index": 0, "finish_reason": finish,
            "message": {"role": "assistant", "content": text,
                "refusal": (!refusal.is_empty()).then_some(refusal),
                "tool_calls": (!calls.is_empty()).then_some(calls)}}],
    }))
}

pub(super) fn stream(inner: ResponseStream) -> ChatCompletionResponseStream {
    Box::pin(ResponsesStream {
        inner,
        id: String::new(),
        model: String::new(),
        created: 0,
        finished: false,
    })
}

struct ResponsesStream {
    inner: ResponseStream,
    id: String,
    model: String,
    created: u64,
    finished: bool,
}

impl ResponsesStream {
    fn event(
        &mut self,
        event: ResponseStreamEvent,
    ) -> Result<Option<CreateChatCompletionStreamResponse>, OpenAIError> {
        let mut finish = None;
        let mut tokens = Value::Null;
        let delta = match event {
            ResponseStreamEvent::ResponseOutputTextDelta(event) => json!({"content": event.delta}),
            ResponseStreamEvent::ResponseRefusalDelta(event) => json!({"refusal": event.delta}),
            ResponseStreamEvent::ResponseReasoningSummaryTextDelta(event) => {
                json!({"reasoning_content": event.delta})
            }
            ResponseStreamEvent::ResponseReasoningTextDelta(event) => {
                json!({"reasoning_content": event.delta})
            }
            ResponseStreamEvent::ResponseOutputItemAdded(event) => match event.item {
                OutputItem::FunctionCall(call) => json!({"tool_calls": [{
                    "index": event.output_index, "id": call.call_id, "type": "function",
                    "function": {"name": call.name, "arguments": call.arguments},
                }]}),
                OutputItem::Message(_) | OutputItem::Reasoning(_) => return Ok(None),
                _ => return Err(invalid("unsupported Responses output item")),
            },
            ResponseStreamEvent::ResponseFunctionCallArgumentsDelta(event) => {
                json!({"tool_calls": [{
                    "index": event.output_index, "function": {"arguments": event.delta},
                }]})
            }
            ResponseStreamEvent::ResponseCompleted(event) => {
                self.finished = true;
                self.id = event.response.id.clone();
                self.model = event.response.model.clone();
                self.created = event.response.created_at;
                finish = Some(finish_reason(&event.response)?);
                tokens = usage(&event.response)?;
                json!({})
            }
            ResponseStreamEvent::ResponseIncomplete(event) => {
                self.finished = true;
                self.id = event.response.id.clone();
                self.model = event.response.model.clone();
                self.created = event.response.created_at;
                finish = Some(finish_reason(&event.response)?);
                tokens = usage(&event.response)?;
                json!({})
            }
            ResponseStreamEvent::ResponseFailed(event) => {
                self.finished = true;
                return Err(invalid(format!(
                    "Responses API failed: {:?}",
                    event.response.error
                )));
            }
            ResponseStreamEvent::ResponseError(event) => return Err(invalid(event.message)),
            _ => return Ok(None),
        };
        decode(json!({
            "id": self.id, "created": self.created, "model": self.model,
            "object": "chat.completion.chunk", "usage": tokens,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        }))
        .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    fn chat(value: Value) -> CreateChatCompletionRequest {
        serde_json::from_value(value).unwrap()
    }

    fn response(status: &str, output: Value) -> Response {
        serde_json::from_value(json!({
            "id": "resp", "created_at": 42, "object": "response",
            "model": "test", "status": status, "output": output,
            "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5,
                "input_tokens_details": {"cached_tokens": 1},
                "output_tokens_details": {"reasoning_tokens": 1}}
        }))
        .unwrap()
    }

    #[test]
    fn converts_conversation_tools_and_options() {
        let req = request(chat(json!({
            "model": "test", "stream": true, "max_completion_tokens": 123,
            "reasoning_effort": "low", "temperature": 0.7,
            "messages": [
                {"role": "system", "content": "instructions"},
                {"role": "user", "content": [
                    {"type": "text", "text": "describe"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}}
                ]},
                {"role": "assistant", "tool_calls": [
                    {"id": "call", "type": "function",
                        "function": {"name": "lookup", "arguments": "{\"x\":1}"}}
                ]},
                {"role": "tool", "tool_call_id": "call", "content": "result"}
            ],
            "tools": [{"type": "function", "function": {
                "name": "lookup", "parameters": {"type": "object", "properties": {}}
            }}],
            "tool_choice": {"type": "function", "function": {"name": "lookup"}},
            "response_format": {"type": "json_schema", "json_schema": {
                "name": "answer", "strict": true, "schema": {"type": "object"}
            }}
        })))
        .unwrap();
        let value = encode(req).unwrap();
        assert_eq!(value["max_output_tokens"], 123);
        assert_eq!(value["store"], false);
        assert_eq!(value["reasoning"]["effort"], "low");
        assert_eq!(value["input"][0]["role"], "system");
        assert_eq!(value["input"][1]["content"][1]["type"], "input_image");
        assert_eq!(value["input"][2]["type"], "function_call");
        assert_eq!(value["input"][2]["call_id"], "call");
        assert_eq!(value["input"][3]["type"], "function_call_output");
        assert_eq!(value["input"][3]["output"], "result");
        assert_eq!(value["tools"][0]["name"], "lookup");
        assert_eq!(value["tools"][0]["strict"], false);
        assert_eq!(value["tool_choice"]["name"], "lookup");
        assert_eq!(value["text"]["format"]["name"], "answer");
    }

    #[test]
    fn rejects_unsupported_chat_options() {
        for option in [
            json!({"n": 2}),
            json!({"stop": "end"}),
            json!({"presence_penalty": 1.0}),
        ] {
            let mut value =
                json!({"model": "test", "messages": [{"role": "user", "content": "hi"}]});
            value
                .as_object_mut()
                .unwrap()
                .extend(option.as_object().unwrap().clone());
            assert!(request(chat(value)).is_err());
        }
    }

    #[test]
    fn converts_completed_text_tools_usage_and_incomplete_status() {
        let output = json!([
            {"type": "message", "id": "msg", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": "hello", "annotations": []}]},
            {"type": "function_call", "id": "item", "call_id": "call",
                "name": "lookup", "arguments": "{}"}
        ]);
        let result = completed(response("completed", output)).unwrap();
        assert_eq!(result.choices[0].message.content.as_deref(), Some("hello"));
        assert_eq!(
            result.choices[0].finish_reason,
            Some(FinishReason::ToolCalls)
        );
        let value = encode(result).unwrap();
        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["id"],
            "call"
        );
        assert_eq!(value["usage"]["prompt_tokens"], 3);
        assert_eq!(value["usage"]["completion_tokens"], 2);
        assert_eq!(value["usage"]["prompt_tokens_details"]["cached_tokens"], 1);
        let mut incomplete = response("incomplete", json!([]));
        incomplete.incomplete_details = Some(async_openai::types::responses::IncompleteDetails {
            reason: "max_output_tokens".into(),
        });
        assert_eq!(
            completed(incomplete).unwrap().choices[0].finish_reason,
            Some(FinishReason::Length)
        );
        assert!(completed(response("failed", json!([]))).is_err());
    }

    fn event(value: Value) -> Result<ResponseStreamEvent, OpenAIError> {
        Ok(serde_json::from_value(value).unwrap())
    }

    #[tokio::test]
    async fn converts_streaming_text_reasoning_tools_and_terminal_event() {
        let output = json!([{"type": "function_call", "call_id": "call", "name": "lookup", "arguments": "{}"}]);
        let events = vec![
            event(json!({"type": "response.created", "sequence_number": 0,
                "response": response("in_progress", json!([]))})),
            event(
                json!({"type": "response.output_text.delta", "sequence_number": 1,
                "item_id": "msg", "output_index": 0, "content_index": 0, "delta": "hello"}),
            ),
            event(
                json!({"type": "response.reasoning_summary_text.delta", "sequence_number": 2,
                "item_id": "reasoning", "output_index": 1, "summary_index": 0, "delta": "thinking"}),
            ),
            event(
                json!({"type": "response.output_item.added", "sequence_number": 3,
                "output_index": 2, "item": {"type": "function_call", "id": "item",
                    "call_id": "call", "name": "lookup", "arguments": ""}}),
            ),
            event(
                json!({"type": "response.function_call_arguments.delta", "sequence_number": 4,
                "item_id": "item", "output_index": 2, "delta": "{}"}),
            ),
            event(json!({"type": "response.completed", "sequence_number": 5,
                "response": response("completed", output)})),
        ];
        let chunks = stream(Box::pin(tokio_stream::iter(events)))
            .collect::<Vec<_>>()
            .await;
        assert_eq!(chunks.len(), 5);
        let chunks = chunks.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(chunks[0].id, "resp");
        assert_eq!(chunks[0].choices[0].delta.content.as_deref(), Some("hello"));
        assert_eq!(
            chunks[1].choices[0].delta.reasoning_content.as_deref(),
            Some("thinking")
        );
        let call = &chunks[2].choices[0].delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.index, 2);
        assert_eq!(call.id.as_deref(), Some("call"));
        assert_eq!(
            chunks[3].choices[0].delta.tool_calls.as_ref().unwrap()[0]
                .function
                .as_ref()
                .unwrap()
                .arguments
                .as_deref(),
            Some("{}")
        );
        assert_eq!(
            chunks[4].choices[0].finish_reason,
            Some(FinishReason::ToolCalls)
        );
        assert_eq!(chunks[4].usage.as_ref().unwrap().total_tokens, 5);
    }

    #[tokio::test]
    async fn propagates_stream_failure_and_premature_end() {
        for events in [
            vec![],
            vec![event(
                json!({"type": "error", "sequence_number": 0, "message": "failure"}),
            )],
            vec![event(
                json!({"type": "response.failed", "sequence_number": 0,
                "response": response("failed", json!([]))}),
            )],
        ] {
            let mut stream = stream(Box::pin(tokio_stream::iter(events)));
            assert!(stream.next().await.unwrap().is_err());
            assert!(stream.next().await.is_none());
        }
    }
}

impl Stream for ResponsesStream {
    type Item = Result<CreateChatCompletionStreamResponse, OpenAIError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        while !this.finished {
            let event = match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(event))) => event,
                Poll::Ready(Some(Err(error))) => {
                    this.finished = true;
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(None) => {
                    this.finished = true;
                    return Poll::Ready(Some(Err(invalid(
                        "Responses stream ended before a terminal event",
                    ))));
                }
            };
            match event {
                ResponseStreamEvent::ResponseCreated(event) => {
                    this.id = event.response.id;
                    this.model = event.response.model;
                    this.created = event.response.created_at;
                    continue;
                }
                ResponseStreamEvent::ResponseInProgress(_) => continue,
                _ => {}
            }
            match this.event(event) {
                Ok(None) => continue,
                Ok(Some(chunk)) => return Poll::Ready(Some(Ok(chunk))),
                Err(error) => {
                    this.finished = true;
                    return Poll::Ready(Some(Err(error)));
                }
            }
        }
        Poll::Ready(None)
    }
}
