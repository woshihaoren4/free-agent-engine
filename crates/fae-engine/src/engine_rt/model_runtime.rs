use std::fmt::Debug;
use std::sync::Arc;

use async_openai::{
    Client,
    config::{Config, OpenAIConfig},
    types::chat::{CreateChatCompletionRequest, CreateChatCompletionResponse},
};
use fae_agent::{Event, EventType, RuntimeSelectExec, TaskReq, TaskResp, TaskType};
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

    async fn complete(
        &self,
        task: TaskReq<CreateChatCompletionRequest>,
    ) -> fae_agent::Result<TaskResp<CreateChatCompletionResponse>> {
        let TaskReq { ctx, mut meta, req } = task;
        let resp = self
            .client
            .chat()
            .create(req)
            .await
            .map_err(anyhow::Error::from)?;
        meta.publisher = Self::ID.to_string();

        Ok(TaskResp { ctx, meta, resp })
    }
}

#[async_trait::async_trait]
impl<C> RuntimeSelectExec<CreateChatCompletionRequest, CreateChatCompletionResponse, (), ()>
    for ModelRuntime<C>
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
            let result = client.chat().create(req).await.map(|resp| {
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
    ) -> fae_agent::Result<TaskResp<CreateChatCompletionResponse>> {
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

    #[tokio::test]
    #[ignore = "requires OPENAI_API_KEY and network access"]
    async fn test_model_runtime_exec_chat_completion_bits_ut() -> anyhow::Result<()> {
        let runtime = ModelRuntime::default();
        println!("open ai config: {:?}", runtime.client.config());
        let model = std::env::var("FAE_DEFAULT_MODEL").unwrap();
        let request = CreateChatCompletionRequestArgs::default()
            .model(model)
            .messages([ChatCompletionRequestUserMessageArgs::default()
                .content("你好")
                .build()?
                .into()])
            .build()?;
        let task = TaskReq {
            ctx: Ctx::new(Arc::new(ContextNull)),
            meta: TaskMeta {
                id: "task-1".to_string(),
                ty: TaskType::Model,
                ..Default::default()
            },
            req: request,
        };

        runtime.select(TaskType::Model, ()).await?;
        assert!(
            runtime.select(TaskType::Tool, ()).await.is_err(),
            "model runtime must reject non-model tasks"
        );

        let response = runtime.exec(task).await?;
        println!("model response: {:#?}", response.resp);
        assert_eq!(response.meta.publisher, ModelRuntime::<OpenAIConfig>::ID);
        assert!(!response.resp.id.is_empty());
        assert!(
            response
                .resp
                .choices
                .first()
                .and_then(|choice| choice.message.content.as_deref())
                .is_some_and(|content| !content.is_empty()),
            "model response must contain non-empty text"
        );

        Ok(())
    }
}
