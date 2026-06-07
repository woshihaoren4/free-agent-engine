use crate::Message;
use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestToolMessageArgs, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageArgs, ChatCompletionRequestUserMessageContent,
    CreateChatCompletionResponse, CreateChatCompletionStreamResponse, FunctionCall,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Clone, PartialEq, Serialize)]
pub enum ModelResponse {
    Response(CreateChatCompletionResponse),
    StreamResponse(CreateChatCompletionStreamResponse),
}

#[derive(Debug, Deserialize, Clone, PartialEq, Serialize)]
pub struct ToolOut {
    pub tool_call_id: String,
    pub tool_name: String,
    pub output: String,
}
impl ToolOut {
    pub fn new(tool_call_id: String, tool_name: String) -> Self {
        Self {
            tool_call_id,
            tool_name,
            output: "".to_string(),
        }
    }
    pub fn set_output(&mut self, output: String) {
        self.output = output;
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq, Serialize)]
pub enum ChatMsg {
    User(ChatCompletionRequestUserMessage),
    Assistant(ModelResponse),
    Tool(ToolOut),
    Custom(String, String),
}
impl ChatMsg {
    pub fn with_user<T: Into<String>>(
        query: T,
    ) -> Self {
        Self::User(
            ChatCompletionRequestUserMessageArgs::default()
                .content(query.into())
                .build()
                .unwrap(),
        )
    }
}

#[derive(Debug, Default, Deserialize, Clone, PartialEq, Serialize)]
pub struct ToolCall {
    pub index: u32,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: String,
}

pub trait MemoryEntry: Message {
    fn from_openai_msg(msg: ChatMsg) -> Vec<Self>
    where
        Self: Sized;
    // 合并流式响应
    // 返回 (合并后的记录列表, 新增内容)
    fn stream_append(
        list: &mut Vec<Self>,
        chunk: CreateChatCompletionStreamResponse,
    ) -> Option<Self>
    where
        Self: Sized;
    fn title(&self) -> String;
    fn content(&self) -> &str;
    fn to_openai_message(self) -> Option<ChatCompletionRequestMessage>;
    // 是否需要记住该条记录，落盘
    fn is_remember(&self) -> bool {
        false
    }
    fn try_to_tool_call(&self) -> Option<ToolCall> {
        None
    }
}

// ------------------- MemoryEntry 的实现 -------------------

#[derive(Debug, Deserialize, Clone, PartialEq, Serialize)]
pub enum RecordItem {
    Wait,
    UserInput(ChatCompletionRequestUserMessage),
    ModelThought(String),
    ModelOutput(String),
    ToolCall(ToolCall),
    ToolOutput(ToolOut),
    Custom(String, String),
}
#[derive(Debug, Deserialize, Clone, PartialEq, Serialize)]
pub struct Record {
    pub id: String,
    pub item: RecordItem,
}
impl Record {
    pub fn from_user_input<S: Into<String>>(query: S) -> Self {
        Self::from(RecordItem::UserInput(
            ChatCompletionRequestUserMessageArgs::default()
                .content(query.into())
                .build()
                .unwrap(),
        ))
    }
    pub fn is_wait(&self) -> bool {
        matches!(self.item, RecordItem::Wait)
    }

    pub fn is_tool_call(&self) -> bool {
        match &self.item {
            RecordItem::ToolCall(_) => true,
            _ => false,
        }
    }
    pub fn reset_id_uuid_v4(mut self) -> Self {
        self.id = wd_tools::uuid::v4();
        self
    }
    pub fn set_model_output(&mut self, output: String) {
        self.item = RecordItem::ModelOutput(output);
    }
    pub fn set_model_thought(&mut self, output: String) {
        self.item = RecordItem::ModelThought(output);
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

impl Message for Record {
    fn id(&self) -> &str {
        &self.id
    }
}

impl MemoryEntry for Record {
    fn from_openai_msg(msg: ChatMsg) -> Vec<Self> {
        let mut msgs = vec![];
        match msg {
            ChatMsg::User(m) => {
                msgs.push(Record {
                    id: wd_tools::uuid::v4(),
                    item: RecordItem::UserInput(m),
                });
            }
            ChatMsg::Assistant(m) => match m {
                ModelResponse::Response(m) => {
                    for i in m.choices {
                        if let Some(t) = i.message.content {
                            msgs.push(Record::from(RecordItem::ModelOutput(t)));
                        }

                        if let Some(t) = i.message.tool_calls {
                            for j in t {
                                match j {
                                    ChatCompletionMessageToolCalls::Function(f) => {
                                        msgs.push(Record::from(RecordItem::ToolCall(ToolCall {
                                            index: i.index,
                                            tool_call_id: f.id,
                                            tool_name: f.function.name,
                                            arguments: f.function.arguments,
                                        })));
                                    }
                                    ChatCompletionMessageToolCalls::Custom(c) => {
                                        msgs.push(Record::from(RecordItem::ToolCall(ToolCall {
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
                ModelResponse::StreamResponse(m) => {
                    Self::stream_append(&mut msgs, m);
                }
            },

            ChatMsg::Tool(m) => {
                msgs.push(Record::from(RecordItem::ToolOutput(m)));
            }

            ChatMsg::Custom(a, b) => {
                msgs.push(Record::from(RecordItem::Custom(a, b)));
            }
        }

        msgs
    }

    fn stream_append(
        list: &mut Vec<Self>,
        chunk: CreateChatCompletionStreamResponse,
    ) -> Option<Self>
    where
        Self: Sized,
    {
        let mut exists = false;
        let mut new_msg = Self::default();
        for i in chunk.choices {
            if let Some(txt) = i.delta.reasoning_content {
                if !txt.is_empty() {
                    new_msg.set_model_thought(txt.clone());
                    exists = false;
                    //存在记录则合并
                    for i in list.iter_mut() {
                        if let RecordItem::ModelThought(t) = &mut i.item {
                            t.push_str(txt.as_str());
                            exists = true;
                            break;
                        }
                    }
                    //不存在记录则新增
                    if !exists {
                        list.push(Record::from(RecordItem::ModelThought(txt)));
                    }
                }
            }
            if let Some(txt) = i.delta.content {
                if !txt.is_empty() {
                    new_msg.set_model_output(txt.clone());
                    exists = false;
                    //存在记录则合并
                    for i in list.iter_mut() {
                        if let RecordItem::ModelOutput(t) = &mut i.item {
                            t.push_str(txt.as_str());
                            exists = true;
                            break;
                        }
                    }
                    //不存在记录则新增
                    if !exists {
                        list.push(Record::from(RecordItem::ModelOutput(txt)));
                    }
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
                                break;
                            }
                        }
                    }
                    if !exists {
                        let mut t = ToolCall::default();
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
        //重新排序
        if new_msg.is_wait() {
            None
        } else {
            Some(new_msg)
        }
    }

    fn title(&self) -> String {
        match &self.item {
            RecordItem::Wait => "Waiting".to_string(),
            RecordItem::UserInput(_) => "User".to_string(),
            RecordItem::ModelThought(_) => "Thinking".to_string(),
            RecordItem::ModelOutput(_) => "Outputting".to_string(),
            RecordItem::ToolCall(tc) => format!("CallTool({})", tc.tool_name),
            RecordItem::ToolOutput(to) => format!("ToolOut({})", to.tool_name),
            RecordItem::Custom(title, _) => title.to_string(),
        }
    }

    fn content(&self) -> &str {
        match &self.item {
            RecordItem::UserInput(m) => match &m.content {
                ChatCompletionRequestUserMessageContent::Text(t) => t.as_str(),
                ChatCompletionRequestUserMessageContent::Array(_) => "",
            },
            RecordItem::ModelThought(t) => t.as_str(),
            RecordItem::ModelOutput(t) => t.as_str(),
            RecordItem::ToolCall(tc) => tc.arguments.as_str(),
            RecordItem::ToolOutput(to) => to.output.as_str(),
            RecordItem::Custom(_, ctn) => ctn.as_str(),
            RecordItem::Wait => "",
        }
    }

    fn to_openai_message(self) -> Option<ChatCompletionRequestMessage> {
        match self.item {
            RecordItem::UserInput(m) => Some(ChatCompletionRequestMessage::User(m)),
            RecordItem::ModelOutput(text) => ChatCompletionRequestAssistantMessageArgs::default()
                .content(text)
                .build()
                .ok()
                .map(ChatCompletionRequestMessage::Assistant),
            RecordItem::ToolCall(tc) => ChatCompletionRequestAssistantMessageArgs::default()
                .tool_calls(vec![ChatCompletionMessageToolCalls::Function(
                    ChatCompletionMessageToolCall {
                        id: tc.tool_call_id,
                        function: FunctionCall {
                            name: tc.tool_name,
                            arguments: tc.arguments,
                        },
                    },
                )])
                .build()
                .ok()
                .map(ChatCompletionRequestMessage::Assistant),
            RecordItem::ToolOutput(to) => ChatCompletionRequestToolMessageArgs::default()
                .tool_call_id(to.tool_call_id)
                .content(to.output)
                .build()
                .ok()
                .map(ChatCompletionRequestMessage::Tool),
            _ => None,
        }
    }

    fn is_remember(&self) -> bool {
        match &self.item {
            RecordItem::UserInput(_) => true,
            RecordItem::ModelOutput(_) => true,
            RecordItem::ToolCall(_) => false,
            RecordItem::ToolOutput(_) => false,
            _ => false,
        }
    }

    fn try_to_tool_call(&self) -> Option<ToolCall>
    where
        Self: Sized,
    {
        match &self.item {
            RecordItem::ToolCall(tc) => Some(tc.clone()),
            _ => None,
        }
    }
}
