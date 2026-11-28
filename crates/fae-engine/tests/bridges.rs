use fae_agent::plugin::bridge::*;
use fae_agent::*;
use fae_engine::EngineBuilder;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug)]
struct JsonSelectReject;
#[async_trait::async_trait]
impl Runtime for JsonSelectReject {
    fn id(&self) -> &str {
        "json_reject"
    }
}
#[tokio::test]
async fn python_select_no_support_keeps_native_condition_for_fallback() {
    let engine = EngineBuilder::new().build().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let ty = TaskType::Tool;
    let runtime = BridgedRuntime::new(
        Arc::new(JsonSelectReject),
        HashMap::from([(
            (ty.clone(), RuntimeOperation::Select),
            RuntimeContract {
                request: ValueCodec::serde::<String>(),
                response: ValueCodec::serde::<Value>(),
            },
        )]),
    )
    .json_runtime();
    let mut cond: AnyType = Box::new("tool".to_string());
    assert!(matches!(
        runtime.select_with_context(&ctx, ty, &mut cond).await,
        Err(fae_agent::Error::RuntimeNoSupport)
    ));
    assert_eq!(cond.downcast_ref::<String>().unwrap(), "tool");
}

#[tokio::test]
async fn tool_boundary_preserves_consumption_tracking() {
    let engine = EngineBuilder::new().build().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let meta = TaskMeta {
        id: "tool-task".into(),
        ty: TaskType::Tool,
        ..Default::default()
    };
    ctx.invocation_started(&meta).unwrap();
    ctx.set_state(ContextState::Running).unwrap();
    let mut response = TaskResp {
        ctx: ctx.clone(),
        meta,
        resp: ToolResponse::with_result("result".into()),
    }
    .into_response();
    response.track_completion();
    response.track_completion();
    let codec = fae_agent::plugin::leases::tool_contract().response;
    let value = codec.encode(&ctx, response.take_payload()).unwrap();
    assert!(!ctx.state().current().is_terminal());
    let restored = codec.decode(&ctx, value).unwrap();
    let mut tool = *restored.downcast::<ToolResponse>().unwrap();
    assert!(
        matches!(tool.next().await.unwrap(), ToolRespItem::Completed(value) if value == "result")
    );
    assert!(matches!(
        &*ctx.state().current(),
        ContextState::Succeeded { .. }
    ));
    assert_eq!(
        ctx.stacks(Default::default())
            .unwrap()
            .iter()
            .filter(|stack| stack.phase == ContextStackPhase::Succeeded)
            .count(),
        1
    );
}

#[tokio::test]
async fn model_stream_reference_is_scoped_and_not_collected() {
    use async_openai::types::chat::CreateChatCompletionStreamResponse;
    let engine = EngineBuilder::new().build().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let meta = TaskMeta {
        id: "model-task".into(),
        ty: TaskType::Model,
        ..Default::default()
    };
    ctx.invocation_started(&meta).unwrap();
    ctx.set_state(ContextState::Running).unwrap();
    let chunk: CreateChatCompletionStreamResponse = serde_json::from_value(json!({
        "id":"chunk","object":"chat.completion.chunk","created":1,"model":"test","choices":[]
    }))
    .unwrap();
    let mut response = TaskResp {
        ctx: ctx.clone(),
        meta,
        resp: ModelResponse::Streaming(Box::pin(tokio_stream::iter([Ok(chunk)]))),
    }
    .into_response();
    response.track_completion();
    let codec = fae_agent::plugin::leases::model_contract().response;
    let value = codec.encode(&ctx, response.take_payload()).unwrap();
    let id = value["stream_id"].as_str().unwrap();
    let other = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    assert!(fae_agent::plugin::leases::next(&other, id).await.is_err());
    assert_eq!(
        fae_agent::plugin::leases::next(&ctx, id).await.unwrap()["value"]["id"],
        "chunk"
    );
    assert!(!ctx.state().current().is_terminal());
    assert_eq!(
        fae_agent::plugin::leases::next(&ctx, id).await.unwrap()["done"],
        true
    );
    assert!(matches!(
        &*ctx.state().current(),
        ContextState::Succeeded { .. }
    ));
}

#[derive(Debug)]
struct Echo;
#[async_trait::async_trait]
impl RuntimeSelectExec<String, String, String, usize> for Echo {
    fn id(&self) -> &str {
        "native_echo"
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Any("echo".into())]
    }
    async fn exec(&self, task: TaskReq<String>) -> fae_agent::Result<TaskResp<String>> {
        Ok(TaskResp {
            ctx: task.ctx,
            meta: task.meta,
            resp: format!("native:{}", task.req),
        })
    }
    async fn select(&self, _: TaskType, cond: String) -> fae_agent::Result<usize> {
        Ok(cond.len())
    }
}
async fn adapters() -> fae_engine::EngineAdapters {
    let mut builder = EngineBuilder::new();
    builder.add_runtime(Echo);
    let ty = TaskType::Any("echo".into());
    builder
        .register_runtime_bridge(
            "native_echo",
            vec![
                (
                    ty.clone(),
                    RuntimeOperation::Exec,
                    RuntimeContract {
                        request: ValueCodec::serde::<String>(),
                        response: ValueCodec::serde::<String>(),
                    },
                    true,
                ),
                (
                    ty,
                    RuntimeOperation::Select,
                    RuntimeContract {
                        request: ValueCodec::serde::<String>(),
                        response: ValueCodec::serde::<usize>(),
                    },
                    true,
                ),
            ],
        )
        .unwrap();
    builder.build_with_adapters().await
}
#[tokio::test]
async fn public_boundary_and_native_calls_preserve_different_types() {
    let adapters = adapters().await;
    let ctx = adapters
        .engine
        .ctx_with_scope(ContextScope::root("alice"))
        .unwrap();
    assert_eq!(
        adapters
            .execute_runtime(
                ctx.clone(),
                "native_echo",
                RuntimeOperation::Exec,
                json!("abc")
            )
            .await
            .unwrap(),
        json!("native:abc")
    );
    let ctx = adapters
        .engine
        .ctx_with_scope(ContextScope::root("alice"))
        .unwrap();
    assert_eq!(
        adapters
            .execute_runtime(
                ctx.clone(),
                "native_echo",
                RuntimeOperation::Select,
                json!("abc")
            )
            .await
            .unwrap(),
        json!(3)
    );
    let result = adapters
        .engine
        .execute_runtime::<String, String>(
            ctx,
            RuntimeCallMeta {
                runtime_id: "native_echo".into(),
                ty: TaskType::Any("echo".into()),
            },
            "typed".into(),
        )
        .await
        .unwrap();
    assert_eq!(result.resp, "native:typed");
}
#[tokio::test]
async fn invalid_json_is_a_boundary_error_and_encoding_checks_actual_type() {
    let adapters = adapters().await;
    let ctx = adapters
        .engine
        .ctx_with_scope(ContextScope::root("alice"))
        .unwrap();
    let error = adapters
        .execute_runtime(
            ctx.clone(),
            "native_echo",
            RuntimeOperation::Exec,
            json!({"text":"abc"}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("string"));
    let codec = ValueCodec::serde::<String>();
    assert!(
        codec
            .encode(&ctx, Box::new(1usize))
            .unwrap_err()
            .to_string()
            .contains("type mismatch")
    );
}
#[test]
fn public_runtime_operations_are_unambiguous() {
    let mut registry = BridgeRegistry::default();
    for name in ["one", "two"] {
        registry
            .register_runtime(
                "multi".into(),
                TaskType::Any(name.into()),
                RuntimeOperation::Exec,
                RuntimeContract {
                    request: ValueCodec::serde::<Value>(),
                    response: ValueCodec::serde::<Value>(),
                },
            )
            .unwrap();
    }
    assert!(
        registry
            .public_runtime("multi", RuntimeOperation::Exec)
            .is_err()
    );
    registry
        .publish_runtime(
            "multi".into(),
            TaskType::Any("two".into()),
            RuntimeOperation::Exec,
        )
        .unwrap();
    assert!(
        registry
            .publish_runtime(
                "multi".into(),
                TaskType::Any("one".into()),
                RuntimeOperation::Exec
            )
            .is_err()
    );
    assert_eq!(
        registry
            .public_runtime("multi", RuntimeOperation::Exec)
            .unwrap(),
        TaskType::Any("two".into())
    );
}

#[derive(Debug)]
struct NativePlanBuilder;
#[async_trait::async_trait]
impl PlanBuilderWithEnv<String> for NativePlanBuilder {
    async fn build(&self, _: RT, ctx: Ctx, env: String) -> anyhow::Result<Box<dyn Plan>> {
        Ok(Box::new(NativePlan { ctx, env }))
    }
}
#[derive(Debug)]
struct NativePlan {
    ctx: Ctx,
    env: String,
}
#[async_trait::async_trait]
impl Plan for NativePlan {
    fn id(&self) -> &str {
        "native_plan"
    }
    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        self.ctx.submit_result(self.env.clone())?;
        Ok(PlanNext::End)
    }
    async fn next(&mut self, _: TaskResponse) -> anyhow::Result<PlanNext> {
        Ok(PlanNext::End)
    }
    async fn abort(&mut self, _: i32, _: String) {}
}
#[tokio::test]
async fn native_plan_accepts_json_only_through_its_registered_boundary() {
    let mut builder = EngineBuilder::new();
    builder.add_plan_builder(NativePlanBuilder);
    builder
        .register_plan_bridge(
            "native_plan",
            to_plan_ty::<String>(),
            ValueCodec::serde::<String>(),
        )
        .unwrap();
    let engine = builder.build().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    engine
        .execute_plan(
            ctx.clone(),
            PlanCallMeta {
                plan_id: "native_plan".into(),
            },
            json!("json env"),
        )
        .await
        .unwrap();
    assert_eq!(ctx.result::<String>().await.unwrap(), "json env");
    let (_, result) = engine
        .invoke::<String, String>("native env".into())
        .await
        .unwrap();
    assert_eq!(result, "native env");
}

#[derive(Debug)]
struct AsyncEcho {
    send: wd_tools::channel::Sender<Event>,
    receive: wd_tools::channel::Receiver<Event>,
}
#[async_trait::async_trait]
impl RuntimeSelectExec<String, String, (), ()> for AsyncEcho {
    fn id(&self) -> &str {
        "async_echo"
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Any("echo".into())]
    }
    async fn watch(&self) -> fae_agent::Result<wd_tools::channel::Receiver<Event>> {
        Ok(self.receive.clone())
    }
    async fn spawn(&self, task: TaskReq<String>) -> fae_agent::Result<()> {
        self.send
            .send(Event {
                from_rt_id: self.id().into(),
                event_type: EventType::TaskResult(
                    TaskResp {
                        ctx: task.ctx,
                        meta: task.meta,
                        resp: task.req,
                    }
                    .into_response(),
                ),
            })
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        Ok(())
    }
}
#[tokio::test]
async fn async_boundary_encodes_results_before_plan_callbacks() {
    let (send, receive) = wd_tools::channel::Channel::new(8);
    let mut builder = EngineBuilder::new();
    builder.add_runtime(AsyncEcho { send, receive });
    builder
        .register_runtime_bridge(
            "async_echo",
            vec![(
                TaskType::Any("echo".into()),
                RuntimeOperation::Exec,
                RuntimeContract {
                    request: ValueCodec::serde::<String>(),
                    response: ValueCodec::serde::<String>(),
                },
                true,
            )],
        )
        .unwrap();
    let engine = builder.build().await;
    let receive = engine.rt().watch().await.unwrap();
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    engine
        .rt()
        .spawn(TaskReq {
            ctx,
            meta: TaskMeta {
                executor: "async_echo".into(),
                ty: TaskType::Any("echo".into()),
                ..Default::default()
            },
            req: json!("async value"),
        })
        .await
        .unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), receive.recv())
        .await
        .unwrap()
        .unwrap();
    let EventType::TaskResult(mut response) = event.event_type else {
        panic!("expected result")
    };
    assert_eq!(
        TaskResp::<Value>::try_from_response(&mut response)
            .unwrap()
            .resp,
        json!("async value")
    );
}

#[derive(Debug)]
struct Decline;
#[async_trait::async_trait]
impl Runtime for Decline {
    fn id(&self) -> &str {
        "decline"
    }
    async fn exec(&self, _: &mut TaskRequest) -> fae_agent::Result<TaskResponse> {
        Err(fae_agent::Error::RuntimeNoSupport)
    }
}
#[tokio::test]
async fn no_support_restores_json_for_the_next_candidate() {
    let ctx = adapters()
        .await
        .engine
        .ctx_with_scope(ContextScope::root("alice"))
        .unwrap();
    let boundary = BridgedRuntime::new(
        Arc::new(Decline),
        HashMap::from([(
            (TaskType::Any("echo".into()), RuntimeOperation::Exec),
            RuntimeContract {
                request: ValueCodec::serde::<String>(),
                response: ValueCodec::serde::<String>(),
            },
        )]),
    );
    let mut task = TaskReq {
        ctx,
        meta: TaskMeta {
            ty: TaskType::Any("echo".into()),
            ..Default::default()
        },
        req: json!("original"),
    }
    .into_request();
    assert!(matches!(
        boundary.exec(&mut task).await,
        Err(fae_agent::Error::RuntimeNoSupport)
    ));
    assert_eq!(
        task.payload().downcast_ref::<Value>().unwrap(),
        &json!("original")
    );
}
