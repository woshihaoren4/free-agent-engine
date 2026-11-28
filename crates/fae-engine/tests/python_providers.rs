#![cfg(feature = "python")]
use fae_agent::{
    ContextScope, PlanCallMeta, RuntimeCallMeta, TaskType,
    hook::hook_py::executor::PythonExecutor,
    plugin::{
        PluginDeclaration, PluginEngine, PluginSnapshot,
        manager::{PluginNode, PluginRecord},
    },
};
use fae_engine::EngineBuilder;
use serde_json::{Value, json};
use std::sync::Arc;

async fn engine() -> (tempfile::TempDir, fae_engine::Engine, Arc<PluginSnapshot>) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("example.py");
    std::fs::write(
        &source,
        r#"
from fae_plugin import ChainPlan, ChainRuntime, PlanNext, TaskRequest, TaskResponse, TaskMetadata
import asyncio
class Report(ChainPlan):
    def id(self): return "report_plan"
    async def init(self):
        if isinstance(self.env, dict) and self.env.get("stream_result"):
            async def items():
                yield "transferred"
            value = await self.ctx.stream(items())
            await self.ctx.complete(value, streams=[value["stream_id"]])
            return PlanNext.End()
        if isinstance(self.env, dict) and self.env.get("hang"):
            await asyncio.sleep(3600)
        if isinstance(self.env, dict) and self.env.get("callbacks"):
            value = await self.rt.select("echo_runtime", {"Any":"echo"}, self.env["value"])
            response = await self.rt.exec(TaskRequest(TaskMetadata("nested-task", {"Any":"echo"},
                executor="echo_runtime"), value, self.ctx))
            value = await self.rt.call_plan("report_plan", response.resp)
            await self.ctx.get()
            await self.ctx.complete(value)
            return PlanNext.End()
        if isinstance(self.env, dict) and self.env.get("native_callbacks"):
            value = await self.rt.select("native_echo", {"Any":"native_echo"}, self.env["value"])
            response = await self.rt.exec(TaskRequest(TaskMetadata("native-task", {"Any":"native_echo"},
                executor="native_echo"), value, self.ctx))
            await self.ctx.complete(response.resp)
            return PlanNext.End()
        await self.ctx.complete(self.env)
        return PlanNext.End()
    async def next(self, response): return PlanNext.End()
    async def abort(self, code, error):
        if isinstance(self.env, dict) and self.env.get("abort_path"):
            from pathlib import Path
            Path(self.env["abort_path"]).write_text(error)
class Tasks(ChainPlan):
    def id(self): return "tasks_plan"
    async def init(self):
        if isinstance(self.env, dict) and self.env.get("native"):
            return PlanNext.Tasks([TaskRequest(TaskMetadata("native-task", {"Any":"native_echo"},
                executor="native_echo"), self.env["value"], None)])
        if isinstance(self.env, dict) and self.env.get("child"):
            return PlanNext.Tasks([TaskRequest(TaskMetadata("child-task", "Plan",
                executor="plan_default", target_plan_id=self.env.get("target", "report_plan")), self.env, None)])
        return PlanNext.Tasks([TaskRequest(TaskMetadata("echo-task", {"Any": "echo"},
            executor="echo_runtime"), self.env, self.ctx)])
    async def next(self, response):
        if isinstance(self.env, dict) and self.env.get("reuse"):
            return PlanNext.Tasks([TaskRequest(TaskMetadata("echo-task", {"Any":"echo"},
                executor="echo_runtime"), self.env, self.ctx)])
        await response.ctx.get()
        await self.ctx.complete(response.resp)
        return PlanNext.End()
class Echo(ChainRuntime):
    def __init__(self): self.count = 0
    def id(self): return "echo_runtime"
    def tys(self): return ["echo"]
    async def select(self, ty, cond):
        if cond == "generation_counter":
            self.count += 1
            return self.count
        return cond
    async def exec(self, task):
        await asyncio.sleep(3600 if isinstance(task.req, dict) and task.req.get("hang") else 0)
        if isinstance(task.req, dict) and task.req.get("stream"):
            async def items():
                try:
                    yield "first"
                    yield "second"
                finally:
                    from pathlib import Path
                    Path(__file__).with_suffix(".stream_closed").write_text("closed")
            value = await self.ctx.stream(items())
            return TaskResponse(task.meta, value, task.ctx)
        return TaskResponse(task.meta, task.req, task.ctx)
    async def exit(self):
        from pathlib import Path
        Path(__file__).with_suffix(".exited").write_text("exit")
class Models(ChainRuntime):
    def id(self): return "python_models"
    def tys(self): return ["Model"]
    async def select(self, ty, cond): return None
    async def exec(self, task):
        if task.req.get("stream"):
            async def chunks():
                yield {"id":"model","object":"chat.completion.chunk","created":1,
                       "model":"test","choices":[]}
            result = await self.ctx.stream(chunks())
        else:
            result = {"kind":"completed", "response":{"id":"model","object":"chat.completion",
                      "created":1,"model":"test","choices":[]}}
        return TaskResponse(task.meta, result, task.ctx)
class Tools(ChainRuntime):
    def id(self): return "python_tools"
    def tys(self): return ["Tool"]
    async def exec(self, task):
        if task.req["tool_name"] == "stream":
            async def items():
                yield {"Streaming":"partial"}
                yield {"Completed":task.req["arguments"]}
            result = await self.ctx.stream(items())
        else:
            result = {"result": task.req["arguments"]}
        return TaskResponse(task.meta, result, task.ctx)
"#,
    )
    .unwrap();
    let declaration = serde_json::from_value::<PluginDeclaration>(json!({
        "id":"example", "plugin_version":"1.0.0", "fae_plugin_api_version":"0.3.0",
        "registration_mode":"chain", "description":"Integration test",
        "mounts":[
            {"point":"Plan", "id":"report_plan", "implementation":"Report"},
            {"point":"Plan", "id":"tasks_plan", "implementation":"Tasks"},
            {"point":"Runtime", "id":"echo_runtime", "implementation":"Echo", "task_types":["echo"]},
            {"point":"Runtime", "id":"python_models", "implementation":"Models", "task_types":["Model"]},
            {"point":"Runtime", "id":"python_tools", "implementation":"Tools", "task_types":["Tool"]}
        ]
    }))
    .unwrap()
    .normalize("example", Some(PluginEngine::Py))
    .unwrap();
    let record = PluginRecord {
        id: "example".into(),
        declaration,
        source,
        revision: "test".into(),
    };
    let executor = PythonExecutor::start(vec![record.clone()]).await.unwrap();
    let node = Arc::new(PluginNode {
        id: "example".into(),
        load_id: "test".into(),
        members: vec![record],
        executor: Arc::new(executor),
    });
    let snapshot = Arc::new(PluginSnapshot {
        nodes: std::collections::BTreeMap::from([("example".into(), node)]),
        order: vec!["example".into()],
    });
    let mut builder = EngineBuilder::new();
    builder.add_plan_builder(ChildScopeProbe);
    builder
        .register_plan_route("child_scope_probe", fae_agent::to_plan_ty::<Value>())
        .unwrap();
    builder.add_runtime(NativeEcho);
    use fae_agent::plugin::bridge::*;
    builder
        .register_runtime_bridge(
            "native_echo",
            vec![
                (
                    TaskType::Any("native_echo".into()),
                    RuntimeOperation::Exec,
                    RuntimeContract {
                        request: ValueCodec::serde::<String>(),
                        response: ValueCodec::serde::<String>(),
                    },
                    true,
                ),
                (
                    TaskType::Any("native_echo".into()),
                    RuntimeOperation::Select,
                    RuntimeContract {
                        request: ValueCodec::serde::<String>(),
                        response: ValueCodec::serde::<String>(),
                    },
                    true,
                ),
            ],
        )
        .unwrap();
    builder.register_plugins(&snapshot).unwrap();
    (directory, builder.build().await, snapshot)
}

#[derive(Debug)]
struct ChildScopeProbe;

#[async_trait::async_trait]
impl fae_agent::PlanBuilderWithEnv<Value> for ChildScopeProbe {
    async fn build(
        &self,
        _: fae_agent::RT,
        ctx: fae_agent::Ctx,
        env: Value,
    ) -> anyhow::Result<Box<dyn fae_agent::Plan>> {
        anyhow::ensure!(
            ctx.scope()?.parent_scope_id.as_deref() == env["expected_parent"].as_str(),
            "JSON child Plan must fork directly from its parent Plan"
        );
        Ok(Box::new(Self))
    }
}

#[async_trait::async_trait]
impl fae_agent::Plan for ChildScopeProbe {
    fn id(&self) -> &str {
        "child_scope_probe"
    }
    async fn init(&mut self) -> anyhow::Result<fae_agent::PlanNext> {
        Ok(fae_agent::PlanNext::End)
    }
    async fn next(&mut self, _: fae_agent::TaskResponse) -> anyhow::Result<fae_agent::PlanNext> {
        unreachable!("probe has no tasks")
    }
    async fn abort(&mut self, _: i32, _: String) {}
}

#[derive(Debug)]
struct NativeEcho;
#[async_trait::async_trait]
impl fae_agent::RuntimeSelectExec<String, String, String, String> for NativeEcho {
    fn id(&self) -> &str {
        "native_echo"
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Any("native_echo".into())]
    }
    async fn select(&self, _: TaskType, cond: String) -> fae_agent::Result<String> {
        Ok(cond)
    }
    async fn exec(
        &self,
        task: fae_agent::TaskReq<String>,
    ) -> fae_agent::Result<fae_agent::TaskResp<String>> {
        Ok(fae_agent::TaskResp {
            ctx: task.ctx,
            meta: task.meta,
            resp: format!("native:{}", task.req),
        })
    }
}

#[tokio::test]
async fn provider_callbacks_cross_native_runtime_boundaries() {
    let (_directory, engine, _) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    engine
        .execute_plan(
            ctx.clone(),
            PlanCallMeta {
                plan_id: "report_plan".into(),
            },
            json!({"native_callbacks":true,"value":"input"}),
        )
        .await
        .unwrap();
    assert_eq!(ctx.result::<Value>().await.unwrap(), json!("native:input"));
}

#[tokio::test]
async fn runtime_exit_does_not_require_a_surviving_execution_context() {
    let (directory, engine, _) = engine().await;
    engine.exit().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.path().join("example.exited")).unwrap(),
        "exit"
    );
    engine.exit().await.unwrap();
}

#[tokio::test]
async fn reused_worker_isolates_engine_generations_and_retires_only_old_runtime() {
    let (_directory, old, snapshot) = engine().await;
    let mut builder = EngineBuilder::new();
    builder.register_plugins(&snapshot).unwrap();
    let new = builder.build().await;
    async fn count(engine: &fae_engine::Engine) -> Value {
        let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
        let result = fae_agent::Engine::select_on(
            engine,
            &ctx,
            RuntimeCallMeta {
                runtime_id: "echo_runtime".into(),
                ty: TaskType::Any("echo".into()),
            },
            Box::new(json!("generation_counter")),
        )
        .await
        .unwrap();
        *result.downcast::<Value>().unwrap()
    }
    assert_eq!(count(&old).await, json!(1));
    assert_eq!(count(&old).await, json!(2));
    assert_eq!(count(&new).await, json!(1));
    old.exit().await.unwrap();
    assert_eq!(count(&new).await, json!(2));
}

#[tokio::test]
async fn last_engine_reference_retires_runtime_without_stopping_shared_worker() {
    let (directory, engine, snapshot) = engine().await;
    drop(engine);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !directory.path().join("example.exited").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut builder = EngineBuilder::new();
    builder.register_plugins(&snapshot).unwrap();
    let engine = builder.build().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let result = engine
        .execute_runtime::<Value, Value>(
            ctx,
            RuntimeCallMeta {
                runtime_id: "echo_runtime".into(),
                ty: TaskType::Any("echo".into()),
            },
            json!("worker still alive"),
        )
        .await
        .unwrap();
    assert_eq!(result.resp, json!("worker still alive"));
}

#[tokio::test]
async fn plan_json_results_and_instances_are_released() {
    let (_directory, engine, snapshot) = engine().await;
    for value in [
        Value::Null,
        json!(true),
        json!("text"),
        json!([1, null]),
        json!({"env":{"payload":7}}),
    ] {
        let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
        engine
            .execute_plan(
                ctx.clone(),
                PlanCallMeta {
                    plan_id: "report_plan".into(),
                },
                value.clone(),
            )
            .await
            .unwrap();
        assert_eq!(ctx.result::<Value>().await.unwrap(), value);
    }
    assert_eq!(
        snapshot.nodes["example"].executor.status()["pending_requests"],
        0
    );
}

#[tokio::test]
async fn runtime_id_calls_and_plan_task_callbacks_preserve_json() {
    let (_directory, engine, _snapshot) = engine().await;
    let ty = TaskType::Any("echo".into());
    let value = json!({"req":["unchanged",null,false]});
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let response = engine
        .execute_runtime::<Value, Value>(
            ctx,
            RuntimeCallMeta {
                runtime_id: "echo_runtime".into(),
                ty: ty.clone(),
            },
            value.clone(),
        )
        .await
        .unwrap();
    assert_eq!(response.resp, value);
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let selected = fae_agent::Engine::select_on(
        &engine,
        &ctx,
        RuntimeCallMeta {
            runtime_id: "echo_runtime".into(),
            ty,
        },
        Box::new(value.clone()),
    )
    .await
    .unwrap();
    assert_eq!(*selected.downcast::<Value>().unwrap(), value);
    engine
        .execute_plan(
            ctx.clone(),
            PlanCallMeta {
                plan_id: "tasks_plan".into(),
            },
            value.clone(),
        )
        .await
        .unwrap();
    assert_eq!(ctx.result::<Value>().await.unwrap(), value);
}

#[tokio::test]
async fn declared_child_plan_uses_registered_id() {
    let (_directory, engine, _snapshot) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    engine
        .execute_plan(
            ctx.clone(),
            PlanCallMeta {
                plan_id: "tasks_plan".into(),
            },
            json!({"child":true}),
        )
        .await
        .unwrap();
    assert_eq!(ctx.result::<Value>().await.unwrap(), Value::Null);
    let stacks = ctx
        .stacks(fae_agent::ContextStackQuery {
            tree: true,
            ..Default::default()
        })
        .unwrap();
    assert!(
        stacks.iter().any(
            |entry| entry.task_meta.origin_plan_id.as_deref() == Some("tasks_plan")
                && entry.task_meta.target_plan_id.as_deref() == Some("report_plan")
        )
    );
}

#[tokio::test]
async fn json_child_plan_forks_directly_from_its_parent() -> anyhow::Result<()> {
    let (_directory, engine, _snapshot) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice"))?;
    let parent_scope = ctx.scope()?.scope_id.clone();
    engine
        .execute_plan(
            ctx.clone(),
            PlanCallMeta {
                plan_id: "tasks_plan".into(),
            },
            json!({"child": true, "target": "child_scope_probe", "expected_parent": parent_scope}),
        )
        .await?;
    assert_eq!(ctx.result::<Value>().await?, Value::Null);
    Ok(())
}

#[tokio::test]
async fn provider_host_callbacks_reenter_engine_without_blocking_worker_reader() {
    let (_directory, engine, _snapshot) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let value = json!({"payload":[null,"nested"]});
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        engine.execute_plan(
            ctx.clone(),
            PlanCallMeta {
                plan_id: "report_plan".into(),
            },
            json!({"callbacks":true,"value":value}),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ctx.result::<Value>().await.unwrap(), value);
}

#[tokio::test]
async fn cancel_waiting_plan_aborts_python_method_and_releases() {
    let (directory, engine, snapshot) = engine().await;
    let abort = directory.path().join("aborted");
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let execution = {
        let engine = engine.clone();
        let ctx = ctx.clone();
        let abort = abort.clone();
        tokio::spawn(async move {
            engine
                .execute_plan(
                    ctx,
                    PlanCallMeta {
                        plan_id: "report_plan".into(),
                    },
                    json!({"hang":true, "abort_path":abort}),
                )
                .await
        })
    };
    // Wait for a host-accepted Plan invocation rather than relying on a fixed delay.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while ctx.stacks(Default::default()).unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    ctx.request_cancel("integration stop".into()).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), execution)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(
        std::fs::read_to_string(abort)
            .unwrap()
            .contains("integration stop")
    );
    assert_eq!(
        snapshot.nodes["example"].executor.status()["pending_requests"],
        0
    );
}

#[tokio::test]
async fn cancel_waiting_runtime_issues_context_bound_kill() {
    let (_directory, engine, snapshot) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let execution = {
        let engine = engine.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            engine
                .execute_runtime::<Value, Value>(
                    ctx,
                    RuntimeCallMeta {
                        runtime_id: "echo_runtime".into(),
                        ty: TaskType::Any("echo".into()),
                    },
                    json!({"hang":true}),
                )
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while snapshot.nodes["example"].executor.status()["pending_requests"] == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    ctx.request_cancel("runtime stop".into()).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), execution)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        snapshot.nodes["example"].executor.status()["pending_requests"],
        0
    );
}

#[tokio::test]
async fn python_stream_survives_rpc_and_closes_on_consumption() {
    let (directory, engine, snapshot) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let result = engine
        .execute_runtime::<Value, Value>(
            ctx.clone(),
            RuntimeCallMeta {
                runtime_id: "echo_runtime".into(),
                ty: TaskType::Any("echo".into()),
            },
            json!({"stream":true}),
        )
        .await
        .unwrap();
    let id = result.resp["stream_id"].as_str().unwrap();
    assert!(!result.ctx.state().current().is_terminal());
    let unrelated = engine.ctx_with_scope(ContextScope::root("bob")).unwrap();
    assert!(
        fae_agent::plugin::leases::next(&unrelated, id)
            .await
            .is_err()
    );
    assert_eq!(
        fae_agent::plugin::leases::next(&ctx, id).await.unwrap()["value"],
        "first"
    );
    assert_eq!(
        fae_agent::plugin::leases::next(&ctx, id).await.unwrap()["value"],
        "second"
    );
    assert_eq!(
        fae_agent::plugin::leases::next(&ctx, id).await.unwrap()["done"],
        true
    );
    assert!(result.ctx.state().current().is_terminal());
    assert!(fae_agent::plugin::leases::next(&ctx, id).await.is_err());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("example.stream_closed")).unwrap(),
        "closed"
    );
    assert_eq!(
        snapshot.nodes["example"].executor.status()["pending_requests"],
        0
    );
}

#[tokio::test]
async fn python_stream_explicit_close_reclaims_iterator() {
    let (directory, engine, _snapshot) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let result = engine
        .execute_runtime::<Value, Value>(
            ctx.clone(),
            RuntimeCallMeta {
                runtime_id: "echo_runtime".into(),
                ty: TaskType::Any("echo".into()),
            },
            json!({"stream":true}),
        )
        .await
        .unwrap();
    let id = result.resp["stream_id"].as_str().unwrap().to_owned();
    fae_agent::plugin::leases::next(&ctx, &id).await.unwrap();
    fae_agent::plugin::leases::close(&ctx, &id).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !directory.path().join("example.stream_closed").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(result.ctx.state().current().is_cancelled());
}

#[tokio::test]
async fn native_model_and_tool_callers_use_python_runtime_contracts() {
    use async_openai::types::chat::CreateChatCompletionRequest;
    use fae_agent::{ModelResponse, ToolRequest, ToolRespItem, ToolResponse};
    use tokio_stream::StreamExt;
    let (_directory, engine, _snapshot) = engine().await;
    for native in [false, true] {
        let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
        let selected = fae_agent::Engine::select_on(
            &engine,
            &ctx,
            RuntimeCallMeta {
                runtime_id: "python_models".into(),
                ty: TaskType::Model,
            },
            if native {
                Box::new(())
            } else {
                Box::new(Value::Null)
            },
        )
        .await
        .unwrap();
        if native {
            assert!(selected.is::<()>());
        } else {
            assert_eq!(*selected.downcast::<Value>().unwrap(), Value::Null);
        }
    }
    for streaming in [false, true] {
        let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
        let req: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model":"test", "messages":[], "stream":streaming
        }))
        .unwrap();
        let result = engine
            .execute_runtime::<_, ModelResponse>(
                ctx,
                RuntimeCallMeta {
                    runtime_id: "python_models".into(),
                    ty: TaskType::Model,
                },
                req,
            )
            .await
            .unwrap();
        if streaming {
            let mut stream = result.resp.into_stream().unwrap();
            assert_eq!(stream.next().await.unwrap().unwrap().id, "model");
            assert!(stream.next().await.is_none());
        } else {
            assert_eq!(result.resp.into_completed().unwrap().id, "model");
        }
        assert!(result.ctx.state().current().is_terminal());
        let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
        let mut result = engine
            .execute_runtime::<_, ToolResponse>(
                ctx,
                RuntimeCallMeta {
                    runtime_id: "python_tools".into(),
                    ty: TaskType::Tool,
                },
                ToolRequest::new(
                    if streaming { "stream" } else { "once" }.into(),
                    "result".into(),
                ),
            )
            .await
            .unwrap();
        assert!(!result.ctx.state().current().is_terminal());
        if streaming {
            assert!(matches!(
                result.resp.next().await.unwrap(),
                ToolRespItem::Streaming(_)
            ));
        }
        assert!(
            matches!(result.resp.next().await.unwrap(), ToolRespItem::Completed(value) if value == "result")
        );
        assert!(result.ctx.state().current().is_terminal());
    }
}

#[tokio::test]
async fn plan_task_id_cannot_be_reused_in_later_batches() {
    let (_directory, engine, _snapshot) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let error = engine
        .execute_plan(
            ctx,
            PlanCallMeta {
                plan_id: "tasks_plan".into(),
            },
            json!({"reuse":true}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already used"));
}

#[tokio::test]
async fn transferred_plan_stream_survives_instance_release() {
    let (_directory, engine, snapshot) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    engine
        .execute_plan(
            ctx.clone(),
            PlanCallMeta {
                plan_id: "report_plan".into(),
            },
            json!({"stream_result":true}),
        )
        .await
        .unwrap();
    let value = ctx.result::<Value>().await.unwrap();
    let id = value["stream_id"].as_str().unwrap();
    assert_eq!(
        fae_agent::plugin::leases::next(&ctx, id).await.unwrap()["value"],
        "transferred"
    );
    assert_eq!(
        fae_agent::plugin::leases::next(&ctx, id).await.unwrap()["done"],
        true
    );
    assert_eq!(
        snapshot.nodes["example"].executor.status()["pending_requests"],
        0
    );
}

#[tokio::test]
async fn consumed_stream_releases_generation_without_waiting_for_deadline() {
    let (directory, engine, snapshot) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let result = engine
        .execute_runtime::<Value, Value>(
            ctx.clone(),
            RuntimeCallMeta {
                runtime_id: "echo_runtime".into(),
                ty: TaskType::Any("echo".into()),
            },
            json!({"stream":true}),
        )
        .await
        .unwrap();
    let id = result.resp["stream_id"].as_str().unwrap();
    while !fae_agent::plugin::leases::next(&ctx, id).await.unwrap()["done"]
        .as_bool()
        .unwrap()
    {}
    drop(result);
    drop(ctx);
    drop(engine);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !directory.path().join("example.exited").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(snapshot.nodes["example"].executor.status()["healthy"], true);
}

#[tokio::test]
async fn runtime_exit_closes_a_stream_after_exec_has_returned() {
    let (directory, engine, _snapshot) = engine().await;
    let ctx = engine.ctx_with_scope(ContextScope::root("alice")).unwrap();
    let result = engine
        .execute_runtime::<Value, Value>(
            ctx.clone(),
            RuntimeCallMeta {
                runtime_id: "echo_runtime".into(),
                ty: TaskType::Any("echo".into()),
            },
            json!({"stream":true}),
        )
        .await
        .unwrap();
    let id = result.resp["stream_id"].as_str().unwrap();
    fae_agent::plugin::leases::next(&ctx, id).await.unwrap();
    engine.exit().await.unwrap();
    assert!(fae_agent::plugin::leases::next(&ctx, id).await.is_err());
    assert!(result.ctx.state().current().is_cancelled());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("example.stream_closed")).unwrap(),
        "closed"
    );
}
