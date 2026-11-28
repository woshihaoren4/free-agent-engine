use super::{connection::Outbound, registry::Registry};
use crate::protocol::ServerConfig;
use fae_agent::{Timestamp, protocol::*};
use std::sync::Arc;
use tokio::sync::watch;

pub fn capabilities() -> EngineCapabilities {
    EngineCapabilities {
        max_history_limit: 100,
        replay: ReplayCapabilities {
            max_events: 4096,
            max_bytes: 16 * 1024 * 1024,
            persistent: false,
        },
        requests: RequestRetention {
            max_entries: 256,
            user_scoped: true,
            persistent: false,
        },
        execution_restore: false,
        full_config_restore: false,
    }
}

pub async fn execute(
    request: &Message,
    config: &ServerConfig,
    registry: &Arc<Registry>,
    shutdown: &watch::Sender<bool>,
) -> anyhow::Result<ResponsePayload> {
    Ok(match request.as_request()? {
        RequestPayload::EngineHello(_) => ResponsePayload::EngineHello(HelloResult {
            api_version: API_VERSION,
            instance: EngineInstance {
                id: config.instance_id.clone(),
                pid: config.pid,
                app_version: config.app_version.clone(),
                started_at: Timestamp::new(&config.started_at)?,
                address: config.address.to_string(),
                port: config.port,
                executable: config.executable.to_string_lossy().into_owned(),
                home: registry.home.to_string_lossy().into_owned(),
            },
            capabilities: capabilities(),
        }),
        RequestPayload::EngineStatus(_) => {
            let (sessions, active_executions) = registry.counts();
            ResponsePayload::EngineStatus(EngineStatus {
                sessions,
                active_executions,
                shutting_down: *shutdown.borrow(),
            })
        }
        RequestPayload::EngineInitialize(args) => {
            let result = super::init::initialize(&registry.home, args).await?;
            ResponsePayload::EngineInitialize(InitializeResult {
                config_path: result.config_path.to_string_lossy().into_owned(),
                skill_count: result.skill_count,
            })
        }
        RequestPayload::EnginePluginShow(args) => {
            // PluginManager's keyed host inspection is converted to the registered public DTO.
            let value = registry.plugins.status(&args.id)?;
            let entries = value
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("invalid plugin status"))?;
            let mut plugins = Vec::with_capacity(entries.len());
            for (id, status) in entries {
                let mut status = status.clone();
                status["id"] = id.clone().into();
                plugins.push(serde_json::from_value::<PluginStatus>(status)?);
            }
            plugins.sort_by_key(|plugin| plugin.order);
            ResponsePayload::EnginePluginShow(PluginShowResult { plugins })
        }
        RequestPayload::EnginePluginInstall(args) => {
            let id = registry
                .plugins
                .install(std::path::Path::new(&args.source))
                .await?;
            ResponsePayload::EnginePluginInstall(PluginMutationResult { id })
        }
        RequestPayload::EnginePluginUninstall(args) => {
            registry.plugins.uninstall(&args.id).await?;
            ResponsePayload::EnginePluginUninstall(PluginMutationResult {
                id: args.id.clone(),
            })
        }
        RequestPayload::PlanCall(payload) => {
            let adapters = registry.current_engine().await?;
            let id = &request
                .plan
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("plan is required"))?
                .plan_id;
            anyhow::ensure!(adapters.bridges.plan(id).is_some(), "plan_not_found: {id}");
            let ctx = adapters
                .engine
                .ctx_with_scope(fae_agent::ContextScope::root(fae_agent::DEFAULT_USER_ID))?;
            adapters
                .engine
                .execute_plan(
                    ctx.clone(),
                    fae_agent::PlanCallMeta {
                        plan_id: id.clone(),
                    },
                    payload.clone(),
                )
                .await?;
            let state = ctx.state().current();
            let fae_agent::ContextState::Succeeded { output } = &*state else {
                anyhow::bail!("Plan did not complete successfully");
            };
            let result = output.json_snapshot()?;
            retain_boundary_result(registry, request, ctx)?;
            ResponsePayload::PlanCall(result)
        }
        RequestPayload::StreamNext(args) | RequestPayload::StreamClose(args) => {
            let ctx = {
                let mut grants = registry.boundary_results.lock().unwrap();
                grants.retain(|_, (deadline, _)| *deadline > std::time::Instant::now());
                grants
                    .get(args.owner_request_id.as_str())
                    .map(|(_, ctx)| ctx.clone())
                    .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?
            };
            if matches!(request.as_request()?, RequestPayload::StreamNext(_)) {
                let result = fae_agent::plugin::leases::next(&ctx, &args.stream_id).await;
                if !fae_agent::plugin::leases::has_owned_resources(&ctx) {
                    registry
                        .boundary_results
                        .lock()
                        .unwrap()
                        .remove(args.owner_request_id.as_str());
                }
                ResponsePayload::StreamNext(result?)
            } else {
                fae_agent::plugin::leases::close(&ctx, &args.stream_id).await?;
                if !fae_agent::plugin::leases::has_owned_resources(&ctx) {
                    registry
                        .boundary_results
                        .lock()
                        .unwrap()
                        .remove(args.owner_request_id.as_str());
                }
                ResponsePayload::StreamClose(Empty {})
            }
        }
        RequestPayload::RuntimeExec(payload) | RequestPayload::RuntimeSelect(payload) => {
            let adapters = registry.current_engine().await?;
            let id = &request
                .call
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("call is required"))?
                .runtime_id;
            let ctx = adapters
                .engine
                .ctx_with_scope(fae_agent::ContextScope::root(fae_agent::DEFAULT_USER_ID))?;
            let operation = if matches!(request.as_request()?, RequestPayload::RuntimeExec(_)) {
                fae_agent::plugin::bridge::RuntimeOperation::Exec
            } else {
                fae_agent::plugin::bridge::RuntimeOperation::Select
            };
            let value = adapters
                .execute_runtime(ctx.clone(), id, operation, payload.clone())
                .await?;
            retain_boundary_result(registry, request, ctx)?;
            if operation == fae_agent::plugin::bridge::RuntimeOperation::Exec {
                ResponsePayload::RuntimeExec(value)
            } else {
                ResponsePayload::RuntimeSelect(value)
            }
        }
        RequestPayload::EngineShutdown(_) => {
            let _ = shutdown.send(true);
            ResponsePayload::EngineShutdown(ShutdownResult { accepted: true })
        }
        _ => anyhow::bail!("engine route does not accept session operations"),
    })
}

fn retain_boundary_result(
    registry: &Registry,
    request: &Message,
    ctx: fae_agent::Ctx,
) -> anyhow::Result<()> {
    if !fae_agent::plugin::leases::has_owned_resources(&ctx) {
        return Ok(());
    }
    let mut grants = registry.boundary_results.lock().unwrap();
    grants.retain(|_, (deadline, _)| *deadline > std::time::Instant::now());
    anyhow::ensure!(grants.len() < 256, "public result capacity exceeded");
    let id = request.request_id.as_ref().unwrap().to_string();
    grants.insert(
        id.clone(),
        (
            std::time::Instant::now() + std::time::Duration::from_secs(120),
            ctx,
        ),
    );
    let grants = registry.boundary_results.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        grants.lock().unwrap().remove(&id);
    });
    Ok(())
}

pub async fn handle(
    text: &str,
    out: &Outbound,
    config: &ServerConfig,
    registry: &Arc<Registry>,
    shutdown: &watch::Sender<bool>,
) {
    let Ok(request) = serde_json::from_str::<RequestMessage>(text)
        .and_then(|r| r.into_message().map_err(serde::de::Error::custom))
    else {
        out.close();
        return;
    };
    if request.as_request().is_err() {
        out.close();
        return;
    }
    let cache = registry.requests_for(fae_agent::DEFAULT_USER_ID);
    let mut requests = cache.lock().await;
    let content = serde_json::to_string(&request).unwrap();
    let rid = request.request_id.as_ref().unwrap().to_string();
    match requests.get(&rid, &content) {
        Ok(Some(response)) => {
            out.send(ServerFrame::Response(response));
            return;
        }
        Err(error) => {
            out.send(ServerFrame::Response(
                ResponseMessage::error(&request, ResponseCode::Conflict, error.to_string())
                    .unwrap(),
            ));
            return;
        }
        Ok(None) => {}
    }
    let is_shutdown = matches!(request.as_request(), Ok(RequestPayload::EngineShutdown(_)));
    let (deferred_shutdown, _) = watch::channel(false);
    let target_shutdown = if is_shutdown {
        &deferred_shutdown
    } else {
        shutdown
    };
    let response = match execute(&request, config, registry, target_shutdown).await {
        Ok(data) => ResponseMessage::success(&request, None, "ok", Some(data)),
        Err(error) => ResponseMessage::error(
            &request,
            if error.to_string().contains("plan_not_found")
                || error
                    .to_string()
                    .contains("Runtime operation is not public")
            {
                ResponseCode::NotFound
            } else if request.session_id.is_some() {
                ResponseCode::BadRequest
            } else {
                ResponseCode::InternalError
            },
            error.to_string(),
        ),
    };
    if let Ok(response) = response {
        requests.push(rid, content, response.clone());
        if is_shutdown {
            if out.send_confirmed(ServerFrame::Response(response)).await {
                let _ = shutdown.send(true);
            }
        } else {
            out.send(ServerFrame::Response(response));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(
        event: &str,
        plan: Option<&str>,
        runtime: Option<&str>,
        payload: serde_json::Value,
    ) -> Message {
        serde_json::from_value::<RequestMessage>(serde_json::json!({
            "api_version":"0.1.0","request_id":"request","event":event,"payload":payload,
            "session":null,"plan":plan.map(|id|serde_json::json!({"plan_id":id})),
            "call":runtime.map(|id|serde_json::json!({"runtime_id":id}))
        }))
        .unwrap()
        .into_message()
        .unwrap()
    }

    #[tokio::test]
    async fn public_provider_calls_reload_transactionally_and_pin_old_generation() {
        use fae_agent::plugin::config::PluginsConfig;
        use serde_json::json;
        let home =
            std::env::temp_dir().join(format!("fae-public-provider-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        let source = home.join("example.py");
        let write = |version: &str, runtime: &str| {
            let declaration = json!({"id":"example","fae_plugin_api_version":"0.3.0","plugin_version":"1.0.0",
                "registration_mode":"chain","description":"test","mounts":[
                {"point":"Plan","id":"report_plan","implementation":"Report"},
                {"point":"Runtime","id":runtime,"implementation":"Echo","task_types":["echo"],
                    "public_calls":{"exec":"echo","select":"echo"}}
            ]});
            std::fs::write(&source, format!(r#"
from fae_plugin import *
def plugin_config(): return {declaration}
class Report(ChainPlan):
    def id(self): return "report_plan"
    async def init(self):
        await self.ctx.complete({{"version":"{version}","value":self.env}})
        return PlanNext.End()
    async def next(self, response): return PlanNext.End()
class Echo(ChainRuntime):
    def id(self): return "{runtime}"
    def tys(self): return ["echo"]
    async def exec(self, task): return TaskResponse(task.meta, {{"version":"{version}","value":task.req}}, task.ctx)
    async def select(self, ty, cond): return {{"version":"{version}","value":cond}}
"#)).unwrap();
        };
        let registry = Registry::new(home.clone(), "test".into());
        registry.initialize_engine().await.unwrap();
        write("one", "echo_runtime");
        registry.plugins.install(&source).await.unwrap();
        let old = registry.current_engine().await.unwrap();
        let old_ctx = old
            .engine
            .ctx_with_scope(fae_agent::ContextScope::root("alice"))
            .unwrap();
        let (out, mut received) = Outbound::test_channel();
        let create = serde_json::from_value::<RequestMessage>(json!({
            "api_version":"0.1.0","request_id":"create","event":"/request/session/create",
            "payload":{"name":"Report","default":true},"session":null,"plan":{"plan_id":"report_plan"}
        })).unwrap().into_message().unwrap();
        let (key, commands) = registry.create(create, None, out.clone()).await.unwrap();
        let _ = received.recv().await.unwrap();
        let handle = registry.state.lock().unwrap().entries[&key.logical()]
            .handle
            .clone()
            .unwrap();
        assert_eq!(
            handle.read_summary().session_config.target,
            fae_agent::PlanTarget::PluginPlan {
                plan_id: "report_plan".into(),
                config: json!({"default":true})
            }
        );
        let (shutdown, _) = watch::channel(false);
        let request = public(
            "/request/plan/call",
            Some("report_plan"),
            None,
            json!({"payload":[1,null]}),
        );
        let result = execute(&request, &config(), &registry, &shutdown)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({"version":"one","value":{"payload":[1,null]}})
        );
        for event in ["/request/runtime/exec", "/request/runtime/select"] {
            let result = execute(
                &public(event, None, Some("echo_runtime"), json!(null)),
                &config(),
                &registry,
                &shutdown,
            )
            .await
            .unwrap();
            assert_eq!(
                serde_json::to_value(result).unwrap(),
                json!({"version":"one","value":null})
            );
        }
        write("two", "echo_runtime");
        registry.plugins.install(&source).await.unwrap();
        let new = registry.current_engine().await.unwrap();
        assert_ne!(
            fae_agent::Engine::generation(&old.engine),
            fae_agent::Engine::generation(&new.engine)
        );
        old.engine
            .execute_plan(
                old_ctx.clone(),
                fae_agent::PlanCallMeta {
                    plan_id: "report_plan".into(),
                },
                json!("old"),
            )
            .await
            .unwrap();
        assert_eq!(
            old_ctx.result::<serde_json::Value>().await.unwrap(),
            json!({"version":"one","value":"old"})
        );
        let result = execute(&request, &config(), &registry, &shutdown)
            .await
            .unwrap();
        assert_eq!(serde_json::to_value(result).unwrap()["version"], "two");
        let mut events = handle.subscribe();
        let start = serde_json::from_value::<RequestMessage>(json!({
            "api_version":"0.1.0","request_id":"start","event":"/request/session/execution/start",
            "payload":{"session_call":true},"session":{"session_id":key.session},"plan":{"plan_id":"report_plan"}
        })).unwrap().into_message().unwrap();
        commands
            .send(super::super::session::Command::Request {
                request: start,
                out: out.clone(),
            })
            .await
            .unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let fae_agent::SessionControlEvent::Finished { outcome, .. } =
                    events.recv().await.unwrap()
                {
                    break outcome;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_value(outcome).unwrap()["result"][0]["value"],
            json!({"version":"two","value":{"session_call":true}})
        );
        let before = registry.plugins.snapshot();
        let before_config = serde_json::to_value(PluginsConfig::read(&home).unwrap()).unwrap();
        write("invalid", fae_engine::SessionRuntime::ID);
        assert!(registry.plugins.install(&source).await.is_err());
        assert!(Arc::ptr_eq(&before, &registry.plugins.snapshot()));
        assert_eq!(
            serde_json::to_value(PluginsConfig::read(&home).unwrap()).unwrap(),
            before_config
        );
        assert_eq!(
            fae_agent::Engine::generation(&new.engine),
            fae_agent::Engine::generation(&registry.current_engine().await.unwrap().engine)
        );
        registry.plugins.uninstall("example").await.unwrap();
        assert!(
            execute(&request, &config(), &registry, &shutdown)
                .await
                .is_err()
        );
        let start = Message::request(
            fae_agent::RequestId::new("unloaded-start").unwrap(),
            Some(fae_agent::SessionId::new(key.session.clone()).unwrap()),
            RequestPayload::SessionExecutionStart(ExecutionStartRequest {
                input: vec![],
                target: None,
                workspace: None,
                options: None,
            }),
        )
        .unwrap();
        commands
            .send(super::super::session::Command::Request {
                request: start,
                out: out.clone(),
            })
            .await
            .unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let ServerFrame::Response(response) =
                    serde_json::from_str::<ServerFrame>(&received.recv().await.unwrap().text)
                        .unwrap()
                {
                    if response.response_id.as_str() == "unloaded-start" {
                        break response;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(response.code, ResponseCode::NotFound);
        assert!(handle.read_summary().active_execution.is_none());
        let ctx = new
            .engine
            .ctx_with_scope(fae_agent::ContextScope::root("alice"))
            .unwrap();
        new.engine
            .execute_plan(
                ctx.clone(),
                fae_agent::PlanCallMeta {
                    plan_id: "report_plan".into(),
                },
                json!("still old"),
            )
            .await
            .unwrap();
        assert_eq!(
            ctx.result::<serde_json::Value>().await.unwrap()["version"],
            "two"
        );
        old.engine.exit().await.unwrap();
        new.engine.exit().await.unwrap();
        registry.shutdown().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    fn config() -> ServerConfig {
        ServerConfig {
            instance_id: "engine-1".into(),
            pid: 42,
            address: "127.0.0.1".parse().unwrap(),
            port: 6666,
            protocol_version: API_VERSION,
            app_version: "test".into(),
            started_at: "2026-11-23T00:00:00Z".into(),
            executable: "/bin/fae".into(),
        }
    }

    #[tokio::test]
    async fn example_plan_stream_is_public_and_survives_generation_reload() {
        use serde_json::json;
        let home =
            std::env::temp_dir().join(format!("fae-provider-stream-{}", wd_tools::uuid::v4()));
        let registry = Registry::new(home.clone(), "stream-test".into());
        registry.initialize_engine().await.unwrap();
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/plugin/plan_runtime");
        registry.plugins.install(&source).await.unwrap();
        let (shutdown, _) = watch::channel(false);
        let request = public(
            "/request/plan/call",
            Some("audit_plan"),
            None,
            json!({"stream":true,"items":["first","second"]}),
        );
        let result = execute(&request, &config(), &registry, &shutdown)
            .await
            .unwrap();
        let value = serde_json::to_value(result).unwrap();
        let id = value["stream_id"].as_str().unwrap();
        // The published grant retains the original Context and Engine generation.
        registry.plugins.install(&source).await.unwrap();
        let stream = |owner: &str, event: &str| {
            public(
                event,
                None,
                None,
                json!({"owner_request_id":owner,"stream_id":id}),
            )
        };
        assert!(
            execute(
                &stream("unrelated", "/request/stream/next"),
                &config(),
                &registry,
                &shutdown
            )
            .await
            .is_err()
        );
        for expected in ["first", "second"] {
            let result = execute(
                &stream("request", "/request/stream/next"),
                &config(),
                &registry,
                &shutdown,
            )
            .await
            .unwrap();
            assert_eq!(serde_json::to_value(result).unwrap()["value"], expected);
        }
        let result = execute(
            &stream("request", "/request/stream/next"),
            &config(),
            &registry,
            &shutdown,
        )
        .await
        .unwrap();
        assert_eq!(serde_json::to_value(result).unwrap()["done"], true);
        assert!(registry.boundary_results.lock().unwrap().is_empty());
        registry.shutdown().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn management_dispatch_returns_registered_payloads_and_real_capabilities() {
        let home = std::env::temp_dir().join(format!("fae-v4-control-{}", wd_tools::uuid::v4()));
        let registry = Registry::new(home.clone(), "engine-1".into());
        let (shutdown, _) = watch::channel(false);
        for payload in [
            RequestPayload::EngineHello(HelloRequest {}),
            RequestPayload::EngineStatus(Empty {}),
            RequestPayload::EnginePluginShow(PluginShowRequest { id: "*".into() }),
            RequestPayload::EngineInitialize(InitializeRequest {
                agent_id: "test".into(),
                model: "test-model".into(),
                force: false,
            }),
            RequestPayload::EngineShutdown(Empty {}),
        ] {
            let request =
                Message::request(fae_agent::RequestId::new("request").unwrap(), None, payload)
                    .unwrap();
            let data = execute(&request, &config(), &registry, &shutdown)
                .await
                .unwrap();
            let response = ResponseMessage::success(&request, None, "ok", Some(data)).unwrap();
            let encoded = serde_json::to_value(&response).unwrap();
            assert_eq!(encoded["code"], 0);
            assert!(!encoded.as_object().unwrap().contains_key("instance_id"));
            let decoded: ResponseMessage = serde_json::from_value(encoded).unwrap();
            decoded.validate_for(&request).unwrap();
            if let Some(ResponsePayload::EngineHello(hello)) = decoded.decode_for(&request).unwrap()
            {
                assert_eq!(hello.instance.id, "engine-1");
                assert!(!hello.capabilities.execution_restore);
                assert!(hello.capabilities.requests.user_scoped);
            }
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn session_requests_are_not_executed_on_the_engine_route() {
        let registry = Registry::new(std::env::temp_dir(), "engine-1".into());
        let (shutdown, _) = watch::channel(false);
        let request = Message::request(
            fae_agent::RequestId::new("request").unwrap(),
            Some(fae_agent::SessionId::new("session").unwrap()),
            RequestPayload::SessionGet(Empty {}),
        )
        .unwrap();
        assert!(
            execute(&request, &config(), &registry, &shutdown)
                .await
                .is_err()
        );
        let response =
            ResponseMessage::error(&request, ResponseCode::BadRequest, "engine route").unwrap();
        assert!(response.payload.is_null());
    }
}
