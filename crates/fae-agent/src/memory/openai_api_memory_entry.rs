use std::collections::HashMap;
use std::fmt::Arguments;
use async_openai::types::chat::{ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls, ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage, ChatCompletionRequestToolMessageArgs, ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageArgs, ChatCompletionRequestUserMessageContent, CreateChatCompletionResponse, CreateChatCompletionStreamResponse, FunctionCall};
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use crate::{MemoryRecord};

#[derive(Debug, Deserialize, Clone, PartialEq, Serialize)]
pub enum OpenAIResponse{
    Response(CreateChatCompletionResponse),
    StreamResponse(CreateChatCompletionStreamResponse),
}

#[derive(Debug, Deserialize, Clone, PartialEq, Serialize)]
pub enum OpenAIChatMsg {
    User(ChatCompletionRequestUserMessage),
    Assistant(OpenAIResponse),
    Tool(RecordItemToolOut),
    Custom(String,String),
}

pub trait OpenAIMemoryEntry: MemoryRecord{
    fn from_openai_msg(msg: OpenAIChatMsg) -> Vec<Self> where Self: Sized;
    // 合并流式响应
    // 返回 (合并后的记录列表, 新增内容)
    fn stream_append(list:&mut Vec<Self>,chunk:CreateChatCompletionStreamResponse) -> Self where Self: Sized;
    fn title(&self) -> String;
    fn content(&self) -> &str;
    fn to_openai_message(self) -> Option<ChatCompletionRequestMessage>;
}

// ------------------- OpenAIMemoryEntry 的实现 -------------------

#[derive(Debug,Default, Deserialize, Clone, PartialEq, Serialize)]
pub struct RecordItemToolCall{
    pub index: u32,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Serialize)]
pub struct RecordItemToolOut{
    pub tool_call_id: String,
    pub tool_name: String,
    pub output: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Serialize)]
pub enum RecordItem {
    Wait,
    UserInput(ChatCompletionRequestUserMessage),
    ModelThought(String),
    ModelOutput(String),
    ToolCall(RecordItemToolCall),
    ToolOutput(RecordItemToolOut),
    Custom(String,String),
}
#[derive(Debug, Deserialize, Clone, PartialEq, Serialize)]
pub struct Record {
    pub id: String,
    pub item: RecordItem,
}
impl Record {
    pub fn from_user_input<S:Into<String>>(query:S)->Self{
        Self::from(RecordItem::UserInput(ChatCompletionRequestUserMessageArgs::default()
            .content(query.into())
            .build().unwrap()))
    }
    pub fn is_wait(&self) -> bool {
        matches!(self.item, RecordItem::Wait)
    }
    pub fn reset_id_uuid_v4(mut self) -> Self {
        self.id = wd_tools::uuid::v4();
        self
    }
    pub fn set_model_output(&mut self, output: String) {
        self.item = RecordItem::ModelOutput(output);
    }
}
impl From<RecordItem> for Record {
    fn from(item: RecordItem) -> Self {
        Self {
            id: "".to_string(),
            item,
        }
    }
}
impl Default for Record {
    fn default() -> Self {
        Self {
            id: "".to_string(),
            item: RecordItem::Wait,
        }
    }
}

impl MemoryRecord for Record {
    fn id(&self) -> &str {
        &self.id
    }
}

impl OpenAIMemoryEntry for Record {

    fn from_openai_msg(msg: OpenAIChatMsg) -> Vec<Self> {
        let mut msgs = vec![];
        match msg {
            OpenAIChatMsg::User(m) => {
                msgs.push(Record {
                    id: wd_tools::uuid::v4(),
                    item: RecordItem::UserInput(m),
                });
            }
            OpenAIChatMsg::Assistant(m) => match m {
                OpenAIResponse::Response(m) => {
                    for i in m.choices {
                        if let Some(t) = i.message.content {
                            msgs.push(Record::from(RecordItem::ModelOutput(t)));
                        }

                        if let Some(t) = i.message.tool_calls {
                            for j in t {
                                match j {
                                    ChatCompletionMessageToolCalls::Function(f) => {
                                        msgs.push(Record::from(RecordItem::ToolCall(RecordItemToolCall {
                                            index: i.index,
                                            tool_call_id: f.id,
                                            tool_name: f.function.name,
                                            arguments: f.function.arguments,
                                        })));
                                    }
                                    ChatCompletionMessageToolCalls::Custom(c) => {
                                        msgs.push(Record::from(RecordItem::ToolCall(RecordItemToolCall {
                                            index: i.index,
                                            tool_call_id: c.id,
                                            tool_name: c.custom_tool.name,
                                            arguments: c.custom_tool.input,
                                        })));
                                    }
                                }

                            }
                        }
                    }
                }
                OpenAIResponse::StreamResponse(m) => {
                    Self::stream_append(&mut msgs,m);
                }
            },

            OpenAIChatMsg::Tool(m) => {
                msgs.push(Record::from(RecordItem::ToolOutput(m)));
            }

            OpenAIChatMsg::Custom(a, b) => {
                msgs.push(Record::from(RecordItem::Custom(a,b)));
            }
        }

        msgs
    }

    fn stream_append(list:&mut Vec<Self>, chunk: CreateChatCompletionStreamResponse) -> Self
    where
        Self: Sized
    {
        println!("---> {:?}",chunk);
        let mut exists = false;
        let mut new_msg = Self::default();
        for i in chunk.choices {
            if let Some(txt) = i.delta.content {
                new_msg.set_model_output(txt.clone());
                exists = false;
                //存在记录则合并
                for i in list.iter_mut() {
                    if let RecordItem::ModelOutput(t) = &mut i.item {
                        t.push_str(txt.as_str());
                        exists = true;
                        break
                    }
                }
                //不存在记录则新增
                if !exists {
                    list.push(Record::from(RecordItem::ModelOutput(txt)));
                }
            }
            if let Some(t) = i.delta.tool_calls {
                for chunk in t {
                    exists = false;
                    //存在记录则合并
                    for i in list.iter_mut() {
                        if let RecordItem::ToolCall(t) = &mut i.item {
                            if t.index == chunk.index {
                                exists = true;
                                if let Some(ref id) = chunk.id {
                                    t.tool_call_id = id.clone();
                                }
                                if let Some(ref func) = chunk.function {
                                    if let Some(ref name) = func.name {
                                        t.tool_name.push_str(name.as_str());
                                    }

                                    if let Some(ref arguments) = func.arguments {
                                        t.arguments.push_str(arguments.as_str());
                                    }
                                }
                                break
                            }
                        }
                    }
                    if !exists {
                        let mut t = RecordItemToolCall::default();
                        t.index = chunk.index;
                        if let Some(id) = chunk.id {
                            t.tool_call_id = id;
                        }
                        if let Some(func) = chunk.function {
                            if let Some(name) = func.name {
                                t.tool_name.push_str(name.as_str());
                            }

                            if let Some(arguments) = func.arguments {
                                t.arguments.push_str(arguments.as_str());
                            }
                        }
                        list.push(Record::from(RecordItem::ToolCall(t)));
                    }
                }
            }
        }
        new_msg
    }


    fn title(&self) -> String {
        match &self.item {
            RecordItem::Wait => "Wait".to_string(),
            RecordItem::UserInput(_) => "User".to_string(),
            RecordItem::ModelThought(_) => "Thought".to_string(),
            RecordItem::ModelOutput(_) => "Assistant".to_string(),
            RecordItem::ToolCall(tc) => format!("ToolCall: {}", tc.tool_name),
            RecordItem::ToolOutput(to) => format!("ToolOutput: {}", to.tool_name),
            RecordItem::Custom(title,_) => title.to_string(),
        }
    }

    fn content(&self) -> &str {
        match &self.item {
            RecordItem::UserInput(m) => match &m.content {
                ChatCompletionRequestUserMessageContent::Text(t) => t.as_str(),
                ChatCompletionRequestUserMessageContent::Array(_) => "",
            },
            RecordItem::ModelThought(t) => t.as_str(),
            RecordItem::ModelOutput(_) => "",
            RecordItem::ToolCall(_) => "",
            RecordItem::ToolOutput(m) => m.output.as_str(),
            RecordItem::Custom(_,ctn) => ctn.as_str(),
            RecordItem::Wait => "",
        }
    }

    fn to_openai_message(self) -> Option<ChatCompletionRequestMessage> {
        match self.item {
            RecordItem::UserInput(m) => Some(ChatCompletionRequestMessage::User(m)),
            RecordItem::ModelOutput(text) => {
                ChatCompletionRequestAssistantMessageArgs::default()
                    .content(text)
                    .build()
                    .ok()
                    .map(ChatCompletionRequestMessage::Assistant)
            }
            RecordItem::ToolCall(tc) => {
                ChatCompletionRequestAssistantMessageArgs::default()
                    .tool_calls(vec![ChatCompletionMessageToolCalls::Function(
                        ChatCompletionMessageToolCall{
                            id: tc.tool_call_id,
                            function: FunctionCall {
                                name: tc.tool_name,
                                arguments: tc.arguments,
                            },
                        }
                    )])
                    .build()
                    .ok()
                    .map(ChatCompletionRequestMessage::Assistant)
            }
            RecordItem::ToolOutput(to) => {
                ChatCompletionRequestToolMessageArgs::default()
                    .tool_call_id(to.tool_call_id)
                    .content(to.output)
                    .build()
                    .ok()
                    .map(ChatCompletionRequestMessage::Tool)
            }
            _ => None,
        }
    }

}