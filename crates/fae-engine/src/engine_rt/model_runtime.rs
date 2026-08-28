use std::{fmt::Debug, sync::Arc};

use async_openai::{
    Client,
    config::{Config, OpenAIConfig},
    types::chat::CreateChatCompletionRequest,
};
use fae_agent::{Event, EventType, ModelResponse, RuntimeSelectExec, TaskReq, TaskResp, TaskType};
use wd_tools::channel::{Channel, Receiver, Sender};

#[derive(Debug)]
pub struct ModelRuntime<C: Config + Debug = OpenAIConfig> {
    client: Arc<Client<C>>,
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Default for ModelRuntime<OpenAIConfig> {
    fn default() -> Self {
        Self::with_client(Client::new())
    }
}

impl ModelRuntime<OpenAIConfig> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: OpenAIConfig) -> Self {
        Self::with_client(Client::with_config(config))
    }
}

impl<C> ModelRuntime<C>
where
    C: Config + Debug + 'static,
{
    pub const ID: &'static str = "model_default";

    pub fn with_client(client: Client<C>) -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            client: Arc::new(client),
            event_sender,
            event_receiver,
        }
    }

    pub fn client(&self) -> &Client<C> {
        self.client.as_ref()
    }

    async fn request(
        client: &Client<C>,
        req: CreateChatCompletionRequest,
    ) -> fae_agent::Result<ModelResponse> {
        if req.stream.unwrap_or(false) {
            let stream = client
                .chat()
                .create_stream(req)
                .await
                .map_err(anyhow::Error::from)?;
            return Ok(ModelResponse::Streaming(stream));
        }

        let response = client
            .chat()
            .create(req)
            .await
            .map_err(anyhow::Error::from)?;
        Ok(ModelResponse::Completed(response))
    }

    async fn complete(
        &self,
        task: TaskReq<CreateChatCompletionRequest>,
    ) -> fae_agent::Result<TaskResp<ModelResponse>> {
        let TaskReq { ctx, mut meta, req } = task;
        let resp = Self::request(self.client.as_ref(), req).await?;
        meta.publisher = Self::ID.to_string();

        Ok(TaskResp { ctx, meta, resp })
    }
}

#[async_trait::async_trait]
impl<C> RuntimeSelectExec<CreateChatCompletionRequest, ModelResponse, (), ()> for ModelRuntime<C>
where
    C: Config + Debug + 'static,
{
    fn id(&self) -> &str {
        Self::ID
    }

    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Model]
    }

    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.event_receiver.clone())
    }

    async fn select(&self, ty: TaskType, _cond: ()) -> fae_agent::Result<()> {
        if ty != TaskType::Model {
            return Err(fae_agent::Error::RuntimeNoSupport);
        }
        Ok(())
    }

    async fn spawn(&self, task: TaskReq<CreateChatCompletionRequest>) -> fae_agent::Result<()> {
        let client = Arc::clone(&self.client);
        let event_sender = self.event_sender.clone();

        tokio::spawn(async move {
            let TaskReq { ctx, mut meta, req } = task;
            let response_ctx = ctx.clone();
            let result = Self::request(client.as_ref(), req).await.map(|resp| {
                meta.publisher = Self::ID.to_string();
                Event {
                    from_rt_id: Self::ID.to_string(),
                    event_type: EventType::TaskResult(
                        TaskResp {
                            ctx: response_ctx,
                            meta,
                            resp,
                        }
                        .into_response(),
                    ),
                }
            });

            match result {
                Ok(event) => {
                    if let Err(err) = event_sender.send(event).await {
                        wd_log::log_error_ln!("send model task result failed: {:?}", err);
                    }
                }
                Err(err) => ctx.error(err.to_string()),
            }
        });

        Ok(())
    }

    async fn exec(
        &self,
        task: TaskReq<CreateChatCompletionRequest>,
    ) -> fae_agent::Result<TaskResp<ModelResponse>> {
        self.complete(task).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::chat::{
        ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs,
    };
    use fae_agent::{ContextNull, Ctx, TaskMeta};
    use tokio_stream::StreamExt;

    fn model_task(stream: bool) -> anyhow::Result<TaskReq<CreateChatCompletionRequest>> {
        let model = std::env::var("FAE_DEFAULT_MODEL")?;
        let request = CreateChatCompletionRequestArgs::default()
            .model(model)
            .stream(stream)
            .messages([ChatCompletionRequestUserMessageArgs::default()
                .content("你好")
                .build()?
                .into()])
            .build()?;

        Ok(TaskReq {
            ctx: Ctx::new(Arc::new(ContextNull)),
            meta: TaskMeta {
                id: "task-1".to_string(),
                ty: TaskType::Model,
                ..Default::default()
            },
            req: request,
        })
    }

    #[tokio::test]
    async fn test_model_runtime_exec_chat_completion_bits_ut() -> anyhow::Result<()> {
        let runtime = ModelRuntime::default();
        println!("open ai config: {:?}", runtime.client.config());

        runtime.select(TaskType::Model, ()).await?;
        assert!(
            runtime.select(TaskType::Tool, ()).await.is_err(),
            "model runtime must reject non-model tasks"
        );

        let response = runtime.exec(model_task(false)?).await?;
        assert_eq!(response.meta.publisher, ModelRuntime::<OpenAIConfig>::ID);
        let ModelResponse::Completed(response) = response.resp else {
            anyhow::bail!("expected completed model response");
        };
        println!("model choices: {:#?}", response.choices);
        assert!(!response.id.is_empty());
        assert!(
            response
                .choices
                .first()
                .and_then(|choice| choice.message.content.as_deref())
                .is_some_and(|content| !content.is_empty()),
            "model response must contain non-empty text"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_model_runtime_exec_streaming_chat_completion_bits_ut() -> anyhow::Result<()> {
        let runtime = ModelRuntime::default();
        let response = runtime.exec(model_task(true)?).await?;
        assert_eq!(response.meta.publisher, ModelRuntime::<OpenAIConfig>::ID);
        let ModelResponse::Streaming(mut stream) = response.resp else {
            anyhow::bail!("expected streaming model response");
        };

        let mut chunks = 0;
        let mut has_content = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            println!("model stream choices: {:#?}", chunk.choices);
            chunks += 1;
            has_content |= chunk.choices.iter().any(|choice| {
                choice
                    .delta
                    .content
                    .as_deref()
                    .is_some_and(|text| !text.is_empty())
            });
        }

        assert!(chunks > 0, "model stream must return at least one chunk");
        assert!(has_content, "model stream must contain non-empty text");
        Ok(())
    }
}
