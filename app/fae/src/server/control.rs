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
            version: VERSION,
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
        RequestPayload::EngineShutdown(_) => {
            let _ = shutdown.send(true);
            ResponsePayload::EngineShutdown(ShutdownResult { accepted: true })
        }
        _ => anyhow::bail!("engine route does not accept session operations"),
    })
}

pub async fn handle(
    text: &str,
    out: &Outbound,
    config: &ServerConfig,
    registry: &Arc<Registry>,
    shutdown: &watch::Sender<bool>,
) {
    let Ok(request) = serde_json::from_str::<Message>(text) else {
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
            if request.session_id.is_some() {
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

    fn config() -> ServerConfig {
        ServerConfig {
            instance_id: "engine-1".into(),
            pid: 42,
            address: "127.0.0.1".parse().unwrap(),
            port: 6666,
            protocol_version: VERSION,
            app_version: "test".into(),
            started_at: "2026-11-23T00:00:00Z".into(),
            executable: "/bin/fae".into(),
        }
    }

    #[tokio::test]
    async fn management_dispatch_returns_registered_payloads_and_real_capabilities() {
        let home = std::env::temp_dir().join(format!("fae-v4-control-{}", wd_tools::uuid::v4()));
        let registry = Registry::new(home.clone(), "engine-1".into());
        let (shutdown, _) = watch::channel(false);
        for payload in [
            RequestPayload::EngineHello(HelloRequest {
                versions: vec![VERSION],
            }),
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
            assert_eq!(encoded["payload"]["code"], 0);
            assert!(!encoded.as_object().unwrap().contains_key("instance_id"));
            let decoded: ResponseMessage = serde_json::from_value(encoded).unwrap();
            assert_eq!(
                decoded.path,
                request.as_request().unwrap().path().response()
            );
            if let Some(ResponsePayload::EngineHello(hello)) = decoded.payload.data {
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
        assert!(response.payload.data.is_none());
    }
}
