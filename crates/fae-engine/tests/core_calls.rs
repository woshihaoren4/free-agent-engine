use fae_agent::*;
use fae_engine::EngineBuilder;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Echo;

#[async_trait::async_trait]
impl RuntimeSelectExec<Value, Value, (), ()> for Echo {
    fn id(&self) -> &str {
        "echo"
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Any("echo".into())]
    }
    async fn exec(&self, task: TaskReq<Value>) -> fae_agent::Result<TaskResp<Value>> {
        Ok(TaskResp {
            ctx: task.ctx,
            meta: task.meta,
            resp: task.req,
        })
    }
}

#[tokio::test]
async fn runtime_calls_keep_prepared_context_and_generation() -> anyhow::Result<()> {
    let mut builder = EngineBuilder::new();
    builder.add_runtime_arc(Arc::new(Echo));
    let engine = builder.build().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice"))?;
    let meta = RuntimeCallMeta {
        runtime_id: "echo".into(),
        ty: TaskType::Any("echo".into()),
    };
    let result = engine
        .clone()
        .execute_runtime::<_, Value>(ctx.clone(), meta.clone(), json!([1, null]))
        .await?;
    assert!(ctx.same_context(&result.ctx));
    assert_eq!(result.resp, json!([1, null]));
    let other = EngineBuilder::new().build().await;
    let foreign = other.ctx_with_scope(ContextScope::root("alice"))?;
    assert!(
        engine
            .execute_runtime::<_, Value>(foreign, meta.clone(), Value::Null)
            .await
            .is_err()
    );
    assert!(
        engine
            .execute_runtime::<_, Value>(Ctx::new(Arc::new(ContextNull)), meta.clone(), Value::Null)
            .await
            .is_err()
    );
    assert!(
        engine
            .execute_runtime::<_, Value>(engine.ctx(), meta.clone(), Value::Null)
            .await
            .is_err()
    );
    assert!(
        engine
            .execute_runtime::<_, Value>(ctx, meta, Value::Null)
            .await
            .is_err()
    );
    engine.exit().await?;
    other.exit().await?;
    Ok(())
}

#[tokio::test]
async fn explicit_runtime_does_not_fall_back_or_accept_other_types() -> anyhow::Result<()> {
    let mut builder = EngineBuilder::new();
    builder.add_runtime(Echo);
    assert!(builder.register_plan_route("example", "env").is_ok());
    assert!(builder.register_plan_route("example", "other").is_err());
    let engine = builder.build().await;
    let dyn_engine: EngineRef = Arc::new(engine.clone());
    assert_eq!(dyn_engine.plans()[0].id, "example");
    assert!(dyn_engine.runtimes().iter().any(|r| r.id == "echo"));
    for meta in [
        RuntimeCallMeta {
            runtime_id: "missing".into(),
            ty: TaskType::Any("echo".into()),
        },
        RuntimeCallMeta {
            runtime_id: "echo".into(),
            ty: TaskType::Model,
        },
    ] {
        let ctx = dyn_engine.create_context(ContextScope::root("alice"))?;
        assert!(
            dyn_engine
                .execute_runtime(ctx, meta, Box::new(Value::Null))
                .await
                .is_err()
        );
    }
    engine.exit().await?;
    Ok(())
}

#[derive(Debug)]
struct LateDecline;

#[async_trait::async_trait]
impl RuntimeSelectExec<String, (), (), ()> for LateDecline {
    fn id(&self) -> &str {
        "late_decline"
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Any("echo".into())]
    }
    fn supports_task(&self, _: &Ctx, _: &TaskMeta, req: &String) -> bool {
        req != "declined before acceptance"
    }
    async fn exec(&self, _: TaskReq<String>) -> fae_agent::Result<TaskResp<()>> {
        Err(Error::RuntimeNoSupport)
    }
    async fn spawn(&self, _: TaskReq<String>) -> fae_agent::Result<()> {
        Err(Error::RuntimeNoSupport)
    }
}

#[derive(Debug)]
struct Fallback(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Runtime for Fallback {
    fn id(&self) -> &str {
        "fallback"
    }
    async fn spawn(&self, task: &mut TaskRequest) -> fae_agent::Result<()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        assert_eq!(task.meta.id, "original-task");
        if let Some(value) = task.payload().downcast_ref::<Value>() {
            assert_eq!(value, &json!(42));
        } else {
            assert_eq!(
                task.payload().downcast_ref::<String>().map(String::as_str),
                Some("declined before acceptance")
            );
        }
        assert_eq!(task.ctx.scope()?.user.user_id.as_str(), "alice");
        assert!(matches!(
            &*task.ctx.state().current(),
            ContextState::Created
        ));
        Ok(())
    }
    async fn exec(&self, task: &mut TaskRequest) -> fae_agent::Result<TaskResponse> {
        self.spawn(task).await?;
        Ok(TaskResp {
            ctx: task.ctx.clone(),
            meta: task.meta.clone(),
            resp: (),
        }
        .into_response())
    }
}

#[tokio::test]
async fn runtime_fallback_requires_an_unaccepted_request() -> anyhow::Result<()> {
    let ty = TaskType::Any("echo".into());
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let mut builder = EngineBuilder::new();
    builder.add_runtime(LateDecline);
    builder.add_runtime_with_tys(Box::new(Fallback(fallback_calls.clone())), [ty.clone()]);
    let engine = builder.build().await;
    let rt = engine.rt();

    for spawn in [false, true] {
        let meta = TaskMeta {
            id: "original-task".into(),
            ty: ty.clone(),
            ..Default::default()
        };
        let ctx = engine.ctx_with_scope(ContextScope::root("alice"))?;
        let mut accepted = TaskReq {
            ctx: ctx.clone(),
            meta: meta.clone(),
            req: String::from("accepted"),
        }
        .into_request();
        let error = if spawn {
            Runtime::spawn(&*rt, &mut accepted).await
        } else {
            Runtime::exec(&*rt, &mut accepted).await.map(|_| ())
        }
        .unwrap_err();
        assert!(
            matches!(error, Error::AcceptedTaskFailed { runtime_id } if runtime_id == "late_decline")
        );
        assert!(matches!(
            &*ctx.state().current(),
            ContextState::Failed { .. }
        ));
    }
    assert_eq!(fallback_calls.load(Ordering::Relaxed), 0);

    for spawn in [false, true] {
        for payload in [
            Box::new(json!(42)) as AnyType,
            Box::new(String::from("declined before acceptance")),
        ] {
            let mut declined = TaskRequest::from_parts(
                engine.ctx_with_scope(ContextScope::root("alice"))?,
                TaskMeta {
                    id: "original-task".into(),
                    ty: ty.clone(),
                    ..Default::default()
                },
                payload,
            );
            if spawn {
                Runtime::spawn(&*rt, &mut declined).await?;
            } else {
                Runtime::exec(&*rt, &mut declined).await?;
            }
        }
    }
    assert_eq!(fallback_calls.load(Ordering::Relaxed), 4);
    Ok(())
}
