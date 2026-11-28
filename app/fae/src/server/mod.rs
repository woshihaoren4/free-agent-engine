mod acceptance;
pub mod connection;
pub mod control;
pub mod engine;
pub mod init;
pub mod lifecycle;
pub mod registry;
pub mod replay;
pub mod runtime;
pub mod session;
pub mod timer;
pub mod workspace;

use crate::protocol::*;
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};
use tokio::{net::TcpListener, sync::watch, task::JoinSet};

pub async fn run(home: PathBuf, address: IpAddr, port: u16) -> anyhow::Result<()> {
    anyhow::ensure!(port > 0, "port 0 is not permitted");
    lifecycle::check_home_owner(&home)?;
    let _lock = lifecycle::FileLock::try_acquire(&home.join("server/server.lock"))?
        .ok_or_else(|| anyhow::anyhow!("service already holds server/server.lock"))?;
    let listener = TcpListener::bind(SocketAddr::new(address, port))
        .await
        .map_err(|e| {
            let advice = if e.kind() == std::io::ErrorKind::PermissionDenied && port < 1024 {
                format!("; this port may require privileges: restart as the home owner with --port {DEFAULT_PORT}")
            } else {
                String::new()
            };
            anyhow::anyhow!(
                "{} {address}:{port}: {e}{advice}",
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    "address_in_use"
                } else {
                    "bind_failed"
                }
            )
        })?;
    let config = ServerConfig {
        instance_id: wd_tools::uuid::v4(),
        pid: std::process::id(),
        address,
        port,
        protocol_version: fae_agent::protocol::API_VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
        started_at: chrono::Utc::now().to_rfc3339(),
        executable: std::env::current_exe()?,
    };
    let token = format!("{}{}", wd_tools::uuid::v4(), wd_tools::uuid::v4());
    let lark_token = format!("{}{}", wd_tools::uuid::v4(), wd_tools::uuid::v4());
    let timer = fae_engine::TimerRuntime::with_home_dir(home.clone()).await?;
    let registry =
        registry::Registry::with_timer(home.clone(), config.instance_id.clone(), timer.handle());
    registry.plugins.load_all().await?;
    registry.initialize_engine().await?;
    let services = timer::ServerServices::start(timer, registry.clone()).await?;
    let (shutdown, mut stopping) = watch::channel(false);
    let signal = lifecycle::stop_signal()?;
    tokio::pin!(signal);
    std::fs::create_dir_all(home.join("lark"))?;
    lifecycle::atomic_write(&home.join("lark/lark.token"), lark_token.as_bytes(), true)?;
    lifecycle::publish(&home, &config, &token)?;
    eprintln!("FAE service ready: PID {} {address}:{port}", config.pid);
    let mut connections = JoinSet::new();
    let mut reap = tokio::time::interval(Duration::from_secs(60));
    let mut logs = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            _ = &mut signal => break,
            _ = stopping.changed() => break,
            _ = reap.tick() => registry.reap().await,
            _ = logs.tick() => {
                if let Err(error) = lifecycle::rotate_running_log(&home) {
                    eprintln!("service log rotation failed: {error:#}");
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let registry = registry.clone();
                let config = config.clone();
                let token = token.clone();
                let lark_token = lark_token.clone();
                let shutdown = shutdown.clone();
                connections.spawn(async move {
                    let Ok((ws, route, principal)) = connection::upgrade(stream, token, lark_token).await else { return };
                    if route == "/api/0.1.0/lark/session" && !peer.ip().is_loopback() { return; }
                    let (out, mut input, transport) = connection::transport(ws);
                    let _guard = connection::ConnectionGuard(out.clone());
                    let mut attached = std::collections::HashMap::new();
                    while let Some(text) = input.recv().await {
                        if route == "/api/0.1.0/control" {
                            control::handle(&text, &out, &config, &registry, &shutdown).await;
                            continue;
                        }
                        use fae_agent::protocol::{RequestMessage, RequestPayload, ResponseMessage, ServerFrame};
                        let request = match serde_json::from_str::<RequestMessage>(&text).and_then(|r| r.into_message().map_err(serde::de::Error::custom)) {
                            Ok(request) => request,
                            // Malformed/other-version frames have no trusted correlation envelope.
                            Err(_) => { out.close(); break; }
                        };
                        let Ok(payload) = request.as_request() else { out.close(); break; };
                        if !payload.event().is_session() {
                            let response = ResponseMessage::error(&request, fae_agent::protocol::ResponseCode::BadRequest, "Expected session operation").unwrap();
                            out.send(ServerFrame::Response(response));
                            continue;
                        }
                        let result = if let RequestPayload::SessionCreate(create) = payload {
                            let validation = if let Some(principal) = &principal {
                                match crate::lark::config::Config::load(&registry.home) {
                                    Ok(config) => match crate::lark::identity::validate(&config, principal, &create.target, false)
                                        .and_then(|_| {
                                            anyhow::ensure!(create.channels.iter().all(|channel| matches!(channel,
                                                fae_agent::ChannelBinding::Lark { app_id, chat_id, thread_id }
                                                if app_id == &principal.app_id && chat_id == &principal.chat_id
                                                    && thread_id.as_deref() == Some(principal.thread_id.as_str()))),
                                                "Channel binding does not match authenticated principal");
                                            Ok(())
                                        }) {
                                        Ok(()) => config.check_agent(&registry.home).await,
                                        Err(error) => Err(error),
                                    },
                                    Err(error) => Err(error),
                                }
                            } else if create.channels.iter().any(|c| matches!(c, fae_agent::ChannelBinding::Lark { .. })) {
                                Err(anyhow::anyhow!("Lark channel requires gateway authentication"))
                            } else { Ok(()) };
                            match validation {
                                Ok(()) => registry.create(request.clone(), principal.clone(), out.clone()).await,
                                Err(error) => Err(ProtocolError::new("forbidden", error)),
                            }
                        } else {
                            let sid = request.session_id.as_ref().unwrap().as_str();
                            match registry.lookup(sid, principal.as_ref()) {
                                Ok((key, tx)) => {
                                    let sent = tx.send(session::Command::Request { request: request.clone(), out: out.clone() }).await;
                                    sent.map(|_| (key, tx)).map_err(|e| ProtocolError::new("session_not_found", e))
                                }
                                Err(error) => Err(error),
                            }
                        };
                        match result {
                            Ok((key, tx)) => { attached.insert(key, tx); }
                            Err(error) => {
                                out.send(ServerFrame::Response(ResponseMessage::error(&request, session::response_code(&error), error.to_string()).unwrap()));
                            }
                        }
                    }
                    for (_, tx) in attached { let _ = tx.send(session::Command::Detach { lease: out.id.clone() }).await; }
                    drop(out);
                    let _ = transport.await;
                });
            }
        }
    }
    drop(listener);
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        services.stop_dispatch().await;
        registry.shutdown().await
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("service shutdown failed: {error:#}");
            return Err(error);
        }
        Err(_) => {
            // Retain discovery files for diagnosing unfinished cleanup.
            anyhow::bail!("service shutdown cleanup exceeded 10 seconds");
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    lifecycle::remove_owned(&home, &config.instance_id)?;
    Ok(())
}
