pub mod connection;
pub mod control;
pub mod engine;
pub mod init;
pub mod lifecycle;
pub mod registry;
pub mod replay;
pub mod runtime;
pub mod session;
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
    let registry = registry::Registry::new(home.clone(), config.instance_id.clone());
    let (shutdown, mut stopping) = watch::channel(false);
    let signal = lifecycle::stop_signal()?;
    tokio::pin!(signal);
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
                let (stream, _) = accepted?;
                let registry = registry.clone();
                let config = config.clone();
                let token = token.clone();
                let shutdown = shutdown.clone();
                connections.spawn(async move {
                    let Ok((ws, route)) = connection::upgrade(stream, token).await else { return };
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
                                ServerMessage::Error(ProtocolError::new("unsupported_version", "Protocol version must be 1"))));
                            break;
                        }
                        if let Some((_, _, tx)) = &binding {
                            let tx: &tokio::sync::mpsc::Sender<session::Command> = tx;
                            let close = matches!(request.body, ClientMessage::Close { .. });
                            if tx.send(session::Command::Request(request)).await.is_err() { break; }
                            if close { break; }
                        } else if let ClientMessage::Open(open) = request.body {
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
    let result = tokio::time::timeout(Duration::from_secs(10), registry.shutdown()).await;
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
