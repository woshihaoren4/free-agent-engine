use std::{net::IpAddr, path::PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerConfig {
    pub instance_id: String,
    pub pid: u32,
    pub address: IpAddr,
    pub port: u16,
    pub protocol_version: u16,
    pub app_version: String,
    pub started_at: String,
    pub executable: PathBuf,
}

impl ServerConfig {
    pub fn endpoint(&self, route: &str) -> String {
        let ip = match self.address {
            IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            ip => ip,
        };
        format!("ws://{}{route}", std::net::SocketAddr::new(ip, self.port))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InitArgsDto {
    pub agent_id: String,
    pub model: String,
    pub force: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ControlMessage {
    Hello,
    Initialize(InitArgsDto),
    Status,
    Shutdown,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ControlEnvelope {
    pub version: u16,
    pub request_id: String,
    #[serde(flatten)]
    pub body: ControlMessage,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HelloInfo {
    pub config: ServerConfig,
    pub home: PathBuf,
}
