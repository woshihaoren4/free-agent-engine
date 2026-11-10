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
    let _lock = lifecycle::FileLock::try_acquire(&home.join("server.lock"))?
        .ok_or_else(|| anyhow::anyhow!("service already holds server.lock"))?;
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
        protocol_version: VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
        started_at: chrono::Utc::now().to_rfc3339(),
        executable: std::env::current_exe()?,
    };
    let token = format!("{}{}", wd_tools::uuid::v4(), wd_tools::uuid::v4());
    let lark_token = format!("{}{}", wd_tools::uuid::v4(), wd_tools::uuid::v4());
    let timer = fae_engine::TimerRuntime::with_home_dir(home.clone()).await?;
    let registry =
        registry::Registry::with_timer(home.clone(), config.instance_id.clone(), timer.handle());
    let services = timer::ServerServices::start(timer, registry.clone()).await?;
    let (shutdown, mut stopping) = watch::channel(false);
    let signal = lifecycle::stop_signal()?;
    tokio::pin!(signal);
    lifecycle::publish(&home, &config, &token)?;
    lifecycle::atomic_write(&home.join("lark.token"), lark_token.as_bytes(), true)?;
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
                    let Ok((ws, route)) = connection::upgrade(stream, token, lark_token).await else { return };
                    if route == "/v3/lark/session" && !peer.ip().is_loopback() { return; }
                    let (out, mut input, transport) = connection::transport(ws);
                    let _guard = connection::ConnectionGuard(out.clone());
                    let mut binding = None;
                    while let Some(text) = input.recv().await {
                        if route == "/v1/control" {
                            control::handle(&text, &out, &config, &registry, &shutdown).await;
                            continue;
                        }
                        let request: ClientEnvelope = match serde_json::from_str(&text) {
                            Ok(request) => request,
                            Err(error) => {
                                out.send(ServerEnvelope::new(&config.instance_id, None, None,
                                    ServerMessage::Error(ProtocolError::new("invalid_message", error))));
                                break;
                            }
                        };
                        if request.version != VERSION {
                            out.send(ServerEnvelope::new(&config.instance_id, None, Some(&request.request_id),
                                ServerMessage::Error(ProtocolError::new("unsupported_version", "Protocol version must be 3"))));
                            break;
                        }
                        if let Some((_, _, tx)) = &binding {
                            if route == "/v3/lark/session" && !matches!(request.body,
                                ClientMessage::ExternalInput { .. } | ClientMessage::Acceptance { .. } |
                                ClientMessage::Clean | ClientMessage::Status | ClientMessage::Cancel { .. } | ClientMessage::Close { .. }) {
                                out.send(ServerEnvelope::new(&config.instance_id, None, Some(&request.request_id),
                                    ServerMessage::Error(ProtocolError::new("forbidden", "Lark route does not allow this operation"))));
                                continue;
                            }
                            let tx: &tokio::sync::mpsc::Sender<session::Command> = tx;
                            let close = matches!(request.body, ClientMessage::Close { .. });
                            if tx.send(session::Command::Request(request)).await.is_err() { break; }
                            if close { break; }
                        } else if let ClientMessage::Open(open) = request.body {
                            let validation = if route == "/v3/lark/session" {
                                match crate::lark::config::Config::load(&registry.home) {
                                    Ok(config) => {
                                        // Registry::open verifies the complete immutable binding
                                        // and rejects expired resumes without creating an actor.
                                        // An existing actor already passed the route check.
                                        match crate::lark::identity::validate(&config, &open) {
                                            Ok(()) if open.resume.is_some() => Ok(()),
                                            Ok(()) => config.check_agent(&registry.home).await,
                                            Err(error) => Err(error),
                                        }
                                    },
                                    Err(error) => Err(error),
                                }
                            } else {
                                if open.binding.is_some() {
                                    Err(anyhow::anyhow!("Explicit binding requires the Lark route"))
                                } else { Ok(()) }
                            };
                            if let Err(error) = validation {
                                out.send(ServerEnvelope::new(&config.instance_id, None, Some(&request.request_id),
                                    ServerMessage::Error(ProtocolError::new("invalid_binding", error))));
                                continue;
                            }
                            match registry.open(open, out.clone(), request.request_id.clone()).await {
                                Ok(bound) => binding = Some(bound),
                                Err(error) => {
                                    out.send(ServerEnvelope::new(&config.instance_id, None, Some(&request.request_id), ServerMessage::Error(error)));
                                }
                            }
                        } else {
                            out.send(ServerEnvelope::new(&config.instance_id, None, Some(&request.request_id),
                                ServerMessage::Error(ProtocolError::new("invalid_message", "First message must be open"))));
                        }
                    }
                    if let Some((_, _, tx)) = binding { let _ = tx.send(session::Command::Detach { lease: out.id.clone() }).await; }
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
