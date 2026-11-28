use fae_agent::{
    ContextCleanup, ContextScope, ContextState, Ctx, Plan, PlanBuilder, PlanBuilderWithEnv,
    PlanCallMeta, PlanNext, RT, TaskResponse,
    plugin::{
        PluginExecutor, PluginValue,
        manager::PluginNode,
        provider::{PluginPlanBuilder, ProviderEndpoint},
    },
    protocol::WorkerRequestMessage,
};
use fae_engine::EngineBuilder;
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;

#[derive(Debug)]
struct Env {
    cleanup_failed: bool,
    released: Arc<AtomicBool>,
}
#[derive(Debug)]
struct Builder;
#[async_trait::async_trait]
impl PlanBuilderWithEnv<Env> for Builder {
    async fn build(&self, _: RT, ctx: Ctx, env: Env) -> anyhow::Result<Box<dyn Plan>> {
        ctx.register_cleanup(ContextCleanup(Box::pin(async move {
            env.released.store(true, Ordering::Release);
            anyhow::ensure!(!env.cleanup_failed, "release failed");
            Ok(())
        })))?;
        Ok(Box::new(ResultPlan(ctx)))
    }
}
#[derive(Debug)]
struct ResultPlan(Ctx);
#[async_trait::async_trait]
impl Plan for ResultPlan {
    fn id(&self) -> &str {
        "result_plan"
    }
    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        self.0.submit_result(serde_json::json!({"ready": true}))?;
        assert!(self.0.submit_result(()).is_err());
        assert!(!self.0.state().current().is_terminal());
        Ok(PlanNext::End)
    }
    async fn next(&mut self, _: TaskResponse) -> anyhow::Result<PlanNext> {
        unreachable!()
    }
    async fn abort(&mut self, _: i32, _: String) {}
}

#[derive(Debug, Default)]
struct CreateWithoutReply {
    created: Mutex<Option<String>>,
    started: Notify,
    released: Mutex<Vec<String>>,
    deferred: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl PluginExecutor for CreateWithoutReply {
    fn reserve_plan_release(
        &self,
        request: WorkerRequestMessage,
    ) -> anyhow::Result<Box<dyn FnOnce() + Send + Sync>> {
        let deferred = self.deferred.clone();
        let instance_id = request.binding.instance_id.unwrap();
        Ok(Box::new(move || deferred.lock().unwrap().push(instance_id)))
    }

    async fn provider(&self, _: &Ctx, request: WorkerRequestMessage) -> anyhow::Result<Value> {
        let instance_id = request.binding.instance_id.unwrap();
        match request.event.as_str() {
            "/request/plugin/plan/create" => {
                *self.created.lock().unwrap() = Some(instance_id);
                self.started.notify_one();
                std::future::pending().await
            }
            "/request/plugin/plan/release" => {
                self.released.lock().unwrap().push(instance_id);
                Ok(Value::Null)
            }
            event => panic!("unexpected event: {event}"),
        }
    }

    async fn invoke(
        &self,
        _: &Ctx,
        _: &Ctx,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: Value,
        _: PluginValue,
    ) -> anyhow::Result<PluginValue> {
        unreachable!("not a hook")
    }
}

#[tokio::test]
async fn cancelled_or_dropped_plan_create_releases_the_preallocated_instance() -> anyhow::Result<()>
{
    for drop_build in [false, true] {
        let executor = Arc::new(CreateWithoutReply::default());
        let plan_builder = PluginPlanBuilder {
            endpoint: ProviderEndpoint {
                node: Arc::new(PluginNode {
                    id: "probe".into(),
                    load_id: "probe".into(),
                    members: Vec::new(),
                    executor: executor.clone(),
                }),
                id: "probe_plan".into(),
                member: "probe:0".into(),
            },
        };
        let mut builder = EngineBuilder::new();
        builder.register_plan_route("probe_plan", plan_builder.ty())?;
        builder.add_raw_plan_builder(Box::new(plan_builder));
        let engine = builder.build().await;
        let ctx = engine.ctx_with_scope(ContextScope::root("alice"))?;
        let mut build = tokio::spawn({
            let ctx = ctx.clone();
            async move {
                engine
                    .execute_plan(
                        ctx,
                        PlanCallMeta {
                            plan_id: "probe_plan".into(),
                        },
                        Value::Null,
                    )
                    .await
            }
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            executor.started.notified(),
        )
        .await?;
        let created = executor.created.lock().unwrap().clone().unwrap();
        if drop_build {
            build.abort();
            assert!(build.await.unwrap_err().is_cancelled());
            assert_eq!(*executor.deferred.lock().unwrap(), [created]);
        } else {
            ctx.request_cancel("cancel while waiting for create response".into())?;
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(2), &mut build).await??;
            let error = result.unwrap_err();
            assert!(matches!(
                error.downcast_ref::<fae_agent::Error>(),
                Some(fae_agent::Error::ContextAborted)
            ));
            assert_eq!(*executor.released.lock().unwrap(), [created]);
        }
    }
    Ok(())
}

#[tokio::test]
async fn cleanup_precedes_success_and_preserves_repeatable_json_result() -> anyhow::Result<()> {
    let mut builder = EngineBuilder::new();
    builder.add_plan_builder(Builder);
    builder.register_plan_route("result_plan", Builder.ty())?;
    let engine = builder.build().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice"))?;
    let released = Arc::new(AtomicBool::new(false));
    engine
        .execute_plan(
            ctx.clone(),
            fae_agent::PlanCallMeta {
                plan_id: "result_plan".into(),
            },
            Env {
                cleanup_failed: false,
                released: released.clone(),
            },
        )
        .await?;
    ctx.terminal().await?;
    assert!(released.load(Ordering::Acquire));
    let ContextState::Succeeded { output } = &*ctx.state().current() else {
        panic!("expected success")
    };
    assert_eq!(output.json_snapshot()?, serde_json::json!({"ready": true}));
    assert_eq!(output.json_snapshot()?, serde_json::json!({"ready": true}));
    assert_eq!(
        ctx.result::<serde_json::Value>().await?,
        serde_json::json!({"ready": true})
    );
    Ok(())
}

#[tokio::test]
async fn cleanup_failure_discards_staged_success() -> anyhow::Result<()> {
    let mut builder = EngineBuilder::new();
    builder.add_plan_builder(Builder);
    builder.register_plan_route("result_plan", Builder.ty())?;
    let engine = builder.build().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice"))?;
    let released = Arc::new(AtomicBool::new(false));
    assert!(
        engine
            .execute_plan(
                ctx.clone(),
                fae_agent::PlanCallMeta {
                    plan_id: "result_plan".into()
                },
                Env {
                    cleanup_failed: true,
                    released: released.clone()
                }
            )
            .await
            .is_err()
    );
    let error = ctx.result::<serde_json::Value>().await.unwrap_err();
    assert!(released.load(Ordering::Acquire));
    assert!(error.to_string().contains("release failed"));
    assert!(matches!(
        &*ctx.state().current(),
        ContextState::Failed { .. }
    ));
    Ok(())
}
