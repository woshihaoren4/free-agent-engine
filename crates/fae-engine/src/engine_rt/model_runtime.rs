use std::{fmt::Debug, sync::Arc, time::Duration};

use async_openai::{
    Client,
    config::{Config, OpenAIConfig},
    error::OpenAIError,
    middleware::{ReqwestService, retry::OpenAIRetryLayer},
    types::chat::CreateChatCompletionRequest,
};
use fae_agent::{
    Event, EventType, ModelResponse, RuntimeSelectExec, TaskError, TaskReq, TaskResp, TaskType,
};
use tower::Layer;
use wd_tools::channel::{Channel, Receiver, Sender};

const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(50),
];

/// Exactly one layer should own retries for a model request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRetryOwner {
    /// Retry transient errors here. The supplied client must disable HTTP retries.
    Runtime,
    /// Make one runtime attempt and let the supplied client manage retries.
    Client,
}

#[derive(Debug)]
pub struct ModelRuntime<C: Config + Debug = OpenAIConfig> {
    client: Arc<Client<C>>,
    retry_owner: ModelRetryOwner,
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Default for ModelRuntime<OpenAIConfig> {
    fn default() -> Self {
        Self::with_config(OpenAIConfig::default())
    }
}

impl ModelRuntime<OpenAIConfig> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: OpenAIConfig) -> Self {
        let service = OpenAIRetryLayer::new(0).layer(ReqwestService::new(reqwest::Client::new()));
        Self::with_client_and_retry_owner(
            Client::with_config(config).with_http_service(service),
            ModelRetryOwner::Runtime,
        )
    }
}

impl<C> ModelRuntime<C>
where
    C: Config + Debug + 'static,
{
    pub const ID: &'static str = "model_default";

    /// Preserve the supplied client's HTTP policy without adding runtime retries.
    pub fn with_client(client: Client<C>) -> Self {
        Self::with_client_and_retry_owner(client, ModelRetryOwner::Client)
    }

    /// Select the retry owner explicitly. For `Runtime`, disable retries in the client.
    pub fn with_client_and_retry_owner(client: Client<C>, retry_owner: ModelRetryOwner) -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            client: Arc::new(client),
            retry_owner,
            event_sender,
            event_receiver,
        }
    }

    pub fn client(&self) -> &Client<C> {
        self.client.as_ref()
    }

    async fn request_once(
        client: &Client<C>,
        req: CreateChatCompletionRequest,
    ) -> Result<ModelResponse, OpenAIError> {
        if req.stream.unwrap_or(false) {
            let stream = client.chat().create_stream(req).await?;
            return Ok(ModelResponse::Streaming(stream));
        }

        let response = client.chat().create(req).await?;
        Ok(ModelResponse::Completed(response))
    }

    async fn request(
        client: &Client<C>,
        req: CreateChatCompletionRequest,
        retry_owner: ModelRetryOwner,
    ) -> fae_agent::Result<ModelResponse> {
        for (retry_index, delay) in RETRY_DELAYS.iter().enumerate() {
            match Self::request_once(client, req.clone()).await {
                Ok(response) => return Ok(response),
                Err(error)
                    if retry_owner == ModelRetryOwner::Client || !Self::is_retryable(&error) =>
                {
                    return Err(anyhow::Error::from(error).into());
                }
                Err(error) => {
                    wd_log::log_warn_ln!(
                        "model request failed, retry {}/{} in {}s: {:?}",
                        retry_index + 1,
                        RETRY_DELAYS.len(),
                        delay.as_secs(),
                        error
                    );
                    tokio::time::sleep(*delay).await;
                }
            }
        }

        Self::request_once(client, req)
            .await
            .map_err(|error| anyhow::Error::from(error).into())
    }

    fn is_retryable(error: &OpenAIError) -> bool {
        match error {
            OpenAIError::ApiError(error) => {
                error.status_code.is_server_error()
                    || (error.status_code == reqwest::StatusCode::TOO_MANY_REQUESTS
                        && error.api_error.r#type.as_deref() != Some("insufficient_quota")
                        && error.api_error.code.as_deref() != Some("insufficient_quota"))
            }
            OpenAIError::Reqwest(error) => error.is_connect() || error.is_timeout(),
            // Invalid requests, decoding errors and stream failures must not replay
            // a request which may already have produced output.
            _ => false,
        }
    }

    async fn complete(
        &self,
        task: TaskReq<CreateChatCompletionRequest>,
    ) -> fae_agent::Result<TaskResp<ModelResponse>> {
        let TaskReq { ctx, mut meta, req } = task;
        let resp = tokio::select! {
            result = Self::request(self.client.as_ref(), req, self.retry_owner) => result?,
            reason = ctx.cancellation() => return Err(anyhow::anyhow!("{}", reason).into()),
        };
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
        let retry_owner = self.retry_owner;
        let event_sender = self.event_sender.clone();

        tokio::spawn(async move {
            let TaskReq { ctx, mut meta, req } = task;
            let _ = ctx.append_stack(fae_agent::ContextStackDraft::new(
                meta.clone(),
                fae_agent::ContextStackPhase::Started,
            ));
            if meta.publisher.is_empty() {
                meta.publisher = Self::ID.to_string();
            }
            let response_ctx = ctx.clone();
            let error_meta = meta.clone();
            let result = tokio::select! {
                result = Self::request(client.as_ref(), req, retry_owner) => result,
                reason = ctx.cancellation() => Err(anyhow::anyhow!("{}", reason).into()),
            }
            .map(|resp| Event {
                from_rt_id: Self::ID.to_string(),
                event_type: EventType::TaskResult(
                    TaskResp {
                        ctx: response_ctx,
                        meta,
                        resp,
                    }
                    .into_response(),
                ),
            });

            match result {
                Ok(event) => {
                    if let Err(err) = event_sender.send(event).await {
                        wd_log::log_error_ln!("send model task result failed: {:?}", err);
                    }
                }
                Err(error) => {
                    let event = Event {
                        from_rt_id: Self::ID.to_string(),
                        event_type: EventType::TaskError(TaskError {
                            ctx,
                            meta: error_meta,
                            error: error.to_string(),
                        }),
                    };
                    if let Err(error) = event_sender.send(event).await {
                        wd_log::log_error_ln!("send model task error failed: {:?}", error);
                    }
                }
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_stream::StreamExt;

    fn offline_request(stream: bool) -> CreateChatCompletionRequest {
        CreateChatCompletionRequestArgs::default()
            .model("offline-model")
            .stream(stream)
            .messages([ChatCompletionRequestUserMessageArgs::default()
                .content("hello")
                .build()
                .unwrap()
                .into()])
            .build()
            .unwrap()
    }

    fn error_body(error_type: &str) -> String {
        serde_json::json!({
            "error": {"message": "test failure", "type": error_type, "code": error_type}
        })
        .to_string()
    }

    fn fake_client(status: u16, body: String, calls: Arc<AtomicUsize>) -> Client<OpenAIConfig> {
        Client::new().with_http_service(tower::service_fn(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            let response: reqwest::Response = http::Response::builder()
                .status(status)
                .body(body.clone())
                .unwrap()
                .into();
            async move { Ok::<_, async_openai::error::OpenAIError>(response) }
        }))
    }

    #[tokio::test]
    async fn test_model_runtime_default_http_attempts_bits_ut() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = calls.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                server_calls.fetch_add(1, Ordering::SeqCst);
                let body = error_body("server_error");
                let response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let runtime = ModelRuntime::with_config(
            OpenAIConfig::default()
                .with_api_base(url)
                .with_api_key("offline-key"),
        );
        for stream in [false, true] {
            calls.store(0, Ordering::SeqCst);
            assert!(
                ModelRuntime::request_once(runtime.client(), offline_request(stream))
                    .await
                    .is_err()
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1, "stream={stream}");
        }
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn test_model_runtime_permanent_errors_bits_ut() {
        for stream in [false, true] {
            for (status, error_type) in [
                (400, "invalid_request_error"),
                (401, "authentication_error"),
                (403, "permission_error"),
                (429, "insufficient_quota"),
            ] {
                let calls = Arc::new(AtomicUsize::new(0));
                let client = fake_client(status, error_body(error_type), calls.clone());
                assert!(
                    ModelRuntime::request(
                        &client,
                        offline_request(stream),
                        ModelRetryOwner::Runtime
                    )
                    .await
                    .is_err()
                );
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    1,
                    "status={status}, stream={stream}"
                );
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let client = fake_client(200, error_body("malformed_response"), calls.clone());
        assert!(
            ModelRuntime::request(&client, offline_request(false), ModelRetryOwner::Runtime)
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let calls = Arc::new(AtomicUsize::new(0));
        let client = fake_client(200, "data: invalid-json\n\n".into(), calls.clone());
        let response =
            ModelRuntime::request(&client, offline_request(true), ModelRetryOwner::Runtime)
                .await
                .unwrap();
        let ModelResponse::Streaming(mut stream) = response else {
            panic!("expected streaming response");
        };
        assert!(stream.next().await.unwrap().is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn test_model_runtime_retry_budget_and_recovery_bits_ut() {
        for stream in [false, true] {
            for status in [429, 503] {
                for failures in [0, 1, 3, 4] {
                    let calls = Arc::new(AtomicUsize::new(0));
                    let service_calls = calls.clone();
                    let client = Client::new().with_http_service(tower::service_fn(move |_| {
                        let attempt = service_calls.fetch_add(1, Ordering::SeqCst);
                        let (status, body) = if attempt < failures {
                            (status, error_body("rate_limit_error"))
                        } else if stream {
                            (200, "data: {\"id\":\"offline\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n".to_string())
                        } else {
                            (200, serde_json::json!({
                                "id": "offline", "object": "chat.completion", "created": 0,
                                "model": "offline", "choices": [{
                                    "index": 0, "message": {"role": "assistant", "content": "done"},
                                    "finish_reason": "stop"
                                }]
                            }).to_string())
                        };
                        let response: reqwest::Response = http::Response::builder()
                            .status(status)
                            .body(body)
                            .unwrap()
                            .into();
                        async move { Ok::<_, OpenAIError>(response) }
                    }));
                    let runtime =
                        ModelRuntime::with_client_and_retry_owner(client, ModelRetryOwner::Runtime);
                    let start = tokio::time::Instant::now();
                    let result = runtime
                        .exec(TaskReq {
                            ctx: Ctx::new(Arc::new(ContextNull)),
                            meta: TaskMeta::default(),
                            req: offline_request(stream),
                        })
                        .await;
                    assert_eq!(calls.load(Ordering::SeqCst), (failures + 1).min(4));
                    let expected_wait: Duration = RETRY_DELAYS.iter().take(failures.min(3)).sum();
                    assert_eq!(start.elapsed(), expected_wait);
                    if failures == 4 {
                        assert!(result.is_err());
                    } else {
                        let response = result.unwrap();
                        assert_eq!(response.meta.publisher, ModelRuntime::<OpenAIConfig>::ID);
                        match response.resp {
                            ModelResponse::Completed(response) => assert_eq!(
                                response.choices[0].message.content.as_deref(),
                                Some("done")
                            ),
                            ModelResponse::Streaming(mut stream) => assert_eq!(
                                stream.next().await.unwrap().unwrap().choices[0]
                                    .delta
                                    .content
                                    .as_deref(),
                                Some("done")
                            ),
                        }
                    }
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_model_runtime_custom_client_owns_retries_bits_ut() {
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime =
            ModelRuntime::with_client(fake_client(503, error_body("server_error"), calls.clone()));
        let start = tokio::time::Instant::now();
        assert!(
            runtime
                .exec(TaskReq {
                    ctx: Ctx::new(Arc::new(ContextNull)),
                    meta: TaskMeta::default(),
                    req: offline_request(false),
                })
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn test_model_runtime_cancellation_stops_retries_bits_ut() {
        for spawned in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let started = Arc::new(tokio::sync::Notify::new());
            let service_calls = calls.clone();
            let service_started = started.clone();
            let client = Client::new().with_http_service(tower::service_fn(move |_| {
                service_calls.fetch_add(1, Ordering::SeqCst);
                service_started.notify_one();
                let response: reqwest::Response = http::Response::builder()
                    .status(503)
                    .body(error_body("server_error"))
                    .unwrap()
                    .into();
                async move { Ok::<_, OpenAIError>(response) }
            }));
            let runtime = Arc::new(ModelRuntime::with_client_and_retry_owner(
                client,
                ModelRetryOwner::Runtime,
            ));
            let ctx = crate::EngineBuilder::new()
                .build()
                .await
                .ctx_with_scope(fae_agent::ContextScope::root("alice"))
                .unwrap();
            let task = TaskReq {
                ctx: ctx.clone(),
                meta: TaskMeta::default(),
                req: offline_request(false),
            };
            let receiver = runtime.watch().await.unwrap();
            let worker_runtime = runtime.clone();
            let worker = tokio::spawn(async move {
                if spawned {
                    worker_runtime.spawn(task).await
                } else {
                    worker_runtime.exec(task).await.map(|_| ())
                }
            });
            started.notified().await;
            ctx.set_state(fae_agent::ContextState::cancelled("stop retries"))
                .unwrap();
            if spawned {
                worker.await.unwrap().unwrap();
                let event = receiver.recv().await.unwrap();
                let EventType::TaskError(error) = event.event_type else {
                    panic!("expected cancellation error");
                };
                assert_eq!(error.error, "stop retries");
            } else {
                assert_eq!(
                    worker.await.unwrap().unwrap_err().to_string(),
                    "stop retries"
                );
            }
            tokio::time::advance(Duration::from_secs(100)).await;
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn test_model_runtime_error_classification_bits_ut() {
        use async_openai::error::{ApiError, ApiErrorResponse};
        for (status, error_type, code, expected) in [
            (429, None, None, true),
            (429, Some("insufficient_quota"), None, false),
            (429, None, Some("insufficient_quota"), false),
            (500, None, None, true),
            (408, None, None, false),
            (401, None, None, false),
        ] {
            let error = OpenAIError::ApiError(ApiErrorResponse {
                status_code: reqwest::StatusCode::from_u16(status).unwrap(),
                api_error: ApiError {
                    message: "offline".into(),
                    r#type: error_type.map(str::to_string),
                    param: None,
                    code: code.map(str::to_string),
                },
            });
            assert_eq!(ModelRuntime::<OpenAIConfig>::is_retryable(&error), expected);
        }
        assert!(!ModelRuntime::<OpenAIConfig>::is_retryable(
            &OpenAIError::InvalidArgument("invalid".into())
        ));
        assert!(!ModelRuntime::<OpenAIConfig>::is_retryable(
            &OpenAIError::StreamError(Box::new(async_openai::error::StreamError::EventStream(
                "closed".into()
            )))
        ));
    }

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
