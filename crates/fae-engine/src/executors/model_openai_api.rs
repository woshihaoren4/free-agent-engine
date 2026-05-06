use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::{ChatCompletionResponseStream, CreateChatCompletionRequest, CreateChatCompletionResponse};
use wd_tools::PFErr;
use fae_agent::{Task, TaskExecutor, TaskResult};

const DEFAULT_OPENAI_MODEL_DESC: &str = "OpenAI API Executor";
const EXECUTOR_OPENAI_API_CHANNEL: &str = "OpenAI_API";


pub struct ModelOpenAIApiExecutorTaskConfig<T>{
    pub req: T,
    pub streaming:bool,
}

pub struct ModelOpenAIApiExecutor{
    pub desc : String,
    pub client: Client<OpenAIConfig>,
}

impl ModelOpenAIApiExecutor {
    pub fn new()->Self{
        let client = Client::new();
        Self{desc:DEFAULT_OPENAI_MODEL_DESC.into(),client}
    }
    pub fn with_config(cfg: OpenAIConfig)->Self {
        let client = Client::with_config(cfg);
        Self{desc:DEFAULT_OPENAI_MODEL_DESC.into(),client}
    }
    pub fn channel()->String{
        EXECUTOR_OPENAI_API_CHANNEL.into()
    }
    pub async fn chat_stream(&self, req:CreateChatCompletionRequest) -> anyhow::Result<ChatCompletionResponseStream> {
        let resp_stream = self.client.chat().create_stream(req).await?;
        Ok(resp_stream)
    }
    pub async fn chat(&self, req:CreateChatCompletionRequest) -> anyhow::Result<CreateChatCompletionResponse> {
        let resp     = self.client.chat().create(req).await?;
        Ok(resp)
    }
}

#[async_trait::async_trait]
impl TaskExecutor for ModelOpenAIApiExecutor {
    fn desc(&self) -> String {
        self.desc.clone()
    }

    fn channel(&self) -> String {
        Self::channel()
    }

    async fn execute(&self, mut task: Task) -> anyhow::Result<TaskResult> {
        if task.assert::<ModelOpenAIApiExecutorTaskConfig<CreateChatCompletionRequest>>() {
            if let Some(req) = task.into_inner::<ModelOpenAIApiExecutorTaskConfig<CreateChatCompletionRequest>>(){
                let result = TaskResult::success(task.id,task.agent_id);
                return if req.streaming {
                    let stream = self.chat_stream(req.req).await?;
                    Ok(result.set_raw_data(stream))
                } else {
                    let resp = self.chat(req.req).await?;
                    Ok(result.set_raw_data(resp))
                }
            }else{
                return anyhow::anyhow!("[ModelOpenAIApiExecutor:execute] parse args failed!").err();
            }
        }else{
            return anyhow::anyhow!("[ModelOpenAIApiExecutor:execute] task args assert failed!").err();
        }
    }
}