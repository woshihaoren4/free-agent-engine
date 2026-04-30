use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::CreateChatCompletionRequest;

pub struct ModelOpenAIApiExecutor{
    pub client: Client<OpenAIConfig>,
}

impl ModelOpenAIApiExecutor {
    pub fn new()->Self{
        let client = Client::new();
        Self{client}
    }
    pub fn with_config(cfg: OpenAIConfig)->Self {
        let client = Client::with_config(cfg);
        Self{client}
    }
    pub fn chat_stream(&self, msg: &str) -> anyhow::Result<String> {
        let req = CreateChatCompletionRequest::default();
        let resp_stream = self.client.chat().create_stream(req).await?;
        let mut resp = String::new();
        for chunk in resp_stream {
            resp += &chunk.choices[0].delta.content;
        }
        Ok(resp)
    }
}