use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionResponseStream, CreateChatCompletionRequest, CreateChatCompletionResponse,
};
use fae_agent::{EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL, Task, TaskExecutor, TaskResult};
use wd_tools::PFErr;

pub const DEFAULT_OPENAI_MODEL_DESC: &str = "OpenAI API Executor";

pub struct ModelOpenAIApiExecutorTaskConfig<T> {
    pub req: T,
    pub streaming: bool,
}

pub struct ModelOpenAIApiExecutor {
    pub desc: String,
    pub channel: String,
    pub client: Client<OpenAIConfig>,
}

impl ModelOpenAIApiExecutor {
    pub fn new() -> Self {
        let client = Client::new();
        Self {
            desc: DEFAULT_OPENAI_MODEL_DESC.into(),
            channel: EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL.into(),
            client,
        }
    }
    pub fn with_config(cfg: OpenAIConfig) -> Self {
        let client = Client::with_config(cfg);
        Self {
            desc: DEFAULT_OPENAI_MODEL_DESC.into(),
            channel: EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL.into(),
            client,
        }
    }
    pub fn set_channel(mut self, channel: &str) -> Self {
        self.channel = channel.into();
        self
    }
    pub fn get_channel(&self) -> String {
        self.channel.clone()
    }
    pub async fn chat_stream(
        &self,
        req: CreateChatCompletionRequest,
    ) -> anyhow::Result<ChatCompletionResponseStream> {
        let resp_stream = self.client.chat().create_stream(req).await?;
        Ok(resp_stream)
    }
    pub async fn chat(
        &self,
        req: CreateChatCompletionRequest,
    ) -> anyhow::Result<CreateChatCompletionResponse> {
        let resp = self.client.chat().create(req).await?;
        Ok(resp)
    }
    pub fn build_stream_chat_request<T>(req: T) -> ModelOpenAIApiExecutorTaskConfig<T> {
        ModelOpenAIApiExecutorTaskConfig {
            req,
            streaming: true,
        }
    }
}

#[async_trait::async_trait]
impl TaskExecutor for ModelOpenAIApiExecutor {
    fn desc(&self) -> String {
        self.desc.clone()
    }

    fn channel(&self) -> String {
        self.get_channel()
    }

    async fn execute(&self, mut task: Task) -> anyhow::Result<TaskResult> {
        if task.assert::<ModelOpenAIApiExecutorTaskConfig<CreateChatCompletionRequest>>() {
            if let Some(req) =
                task.into_inner::<ModelOpenAIApiExecutorTaskConfig<CreateChatCompletionRequest>>()
            {
                let result = TaskResult::success(task.id, task.agent_id);
                return if req.streaming {
                    let stream = self.chat_stream(req.req).await?;
                    Ok(result.set_raw_data(stream))
                } else {
                    let resp = self.chat(req.req).await?;
                    Ok(result.set_raw_data(resp))
                };
            } else {
                return anyhow::anyhow!("[ModelOpenAIApiExecutor:execute] parse args failed!")
                    .err();
            }
        } else if task.assert::<CreateChatCompletionRequest>() {
            if let Some(req) = task.into_inner::<CreateChatCompletionRequest>() {
                let result = TaskResult::success(task.id, task.agent_id);
                if req.stream.unwrap_or(false) {
                    // 流式请求
                    let stream = self.chat_stream(req).await?;
                    return Ok(result.set_raw_data(stream));
                }
                //非流式请求
                let resp = self.chat(req).await?;
                Ok(result.set_raw_data(resp))
            } else {
                return anyhow::anyhow!("[ModelOpenAIApiExecutor:execute] parse args failed!")
                    .err();
            }
        } else {
            return anyhow::anyhow!("[ModelOpenAIApiExecutor:execute] task args assert failed!")
                .err();
        }
    }
}

impl Default for ModelOpenAIApiExecutor {
    fn default() -> Self {
        if let Ok(url) = std::env::var("OPENAI_API_URL") {
            ModelOpenAIApiExecutor::with_config(OpenAIConfig::new().with_api_base(url))
        } else {
            Self::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::chat::{
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
        CreateChatCompletionRequestArgs,
    };
    use fae_agent::TaskType;
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn test_openai_execute() {
        let model = std::env::var("OPENAI_DEFAULT_MODEL").unwrap();
        let cfg = OpenAIConfig::new().with_api_base(std::env::var("OPENAI_API_URL").unwrap());

        let executor = ModelOpenAIApiExecutor::with_config(cfg);

        // ------------> 单次请求 <------------------
        let request = CreateChatCompletionRequestArgs::default()
            .model(model.as_str())
            .max_tokens(512u32)
            .messages([ChatCompletionRequestUserMessageArgs::default()
                .content("简单介绍一下rust语言")
                .build()
                .expect("build message failed!")
                .into()])
            .build()
            .expect("build request failed!");

        let task = Task::new("1", "assistant", TaskType::Model).set_args(request);

        let mut result = executor.execute(task).await.expect("execute failed!");
        let mut answer = result
            .into_inner::<CreateChatCompletionResponse>()
            .expect("get response failed!");
        println!(
            "--->{:?}",
            answer.choices.remove(0).message.content.unwrap()
        );

        // ------------> 流式请求 <------------------
        let request = CreateChatCompletionRequestArgs::default()
            .model(model.as_str())
            .max_tokens(1024u32)
            .messages([
                ChatCompletionRequestSystemMessageArgs::default()
                    .content("你是一个编程小助手")
                    .build()
                    .expect("build message failed!")
                    .into(),
                ChatCompletionRequestUserMessageArgs::default()
                    .content("用rust写一个简单的hello world程序")
                    .build()
                    .expect("build message failed!")
                    .into(),
            ])
            .build()
            .expect("build request failed!");
        let request = ModelOpenAIApiExecutor::build_stream_chat_request(request);
        let task = Task::new("1", "assistant", TaskType::Model).set_args(request);
        let mut result = executor.execute(task).await.expect("execute failed!");
        let mut answer = result
            .into_inner::<ChatCompletionResponseStream>()
            .expect("get response failed!");
        println!("---> start:");
        while let Some(chunk) = answer.next().await {
            print!(
                "{}",
                chunk.unwrap().choices.remove(0).delta.content.unwrap()
            );
        }
        println!("<--- end.");
    }
}
