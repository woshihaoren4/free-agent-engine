use super::{connection::Outbound, registry::Registry};
use crate::protocol::*;
use std::sync::Arc;
use tokio::sync::watch;

pub async fn handle(
    text: &str,
    out: &Outbound,
    config: &ServerConfig,
    registry: &Arc<Registry>,
    shutdown: &watch::Sender<bool>,
) {
    let request: ControlEnvelope = match serde_json::from_str(text) {
        Ok(request) => request,
        Err(e) => {
            out.send(ServerEnvelope::new(
                &config.instance_id,
                None,
                None,
                ServerMessage::Error(ProtocolError::new("invalid_message", e)),
            ));
            return;
        }
    };
    let body = if request.version != VERSION {
        ServerMessage::Error(ProtocolError::new(
            "unsupported_version",
            "Protocol version must be 1",
        ))
    } else {
        match request.body {
            ControlMessage::Hello => ServerMessage::Hello(HelloInfo {
                config: config.clone(),
                home: registry.home.clone(),
            }),
            ControlMessage::Status => {
                let (sessions, active_rounds) = registry.counts();
                ServerMessage::ServiceStatus {
                    sessions,
                    active_rounds,
                }
            }
            ControlMessage::PluginLoad { id } => match registry.plugins.load(&id).await {
                Ok(status) => ServerMessage::Plugins(status),
                Err(error) => ServerMessage::Error(ProtocolError::new("plugin_load_failed", error)),
            },
            ControlMessage::PluginShow { id } => match registry.plugins.status(&id) {
                Ok(status) => ServerMessage::Plugins(status),
                Err(error) => ServerMessage::Error(ProtocolError::new("plugin_show_failed", error)),
            },
            ControlMessage::Initialize(args) => {
                match super::init::initialize(&registry.home, &args).await {
                    Ok(result) => ServerMessage::Initialized {
                        config_path: result.config_path,
                        skill_count: result.skill_count,
                    },
                    Err(e) => ServerMessage::Error(ProtocolError::new("execution_failed", e)),
                }
            }
            ControlMessage::Shutdown => {
                let _ = shutdown.send(true);
                ServerMessage::Shutdown
            }
        }
    };
    out.send(ServerEnvelope::new(
        &config.instance_id,
        None,
        Some(&request.request_id),
        body,
    ));
}
