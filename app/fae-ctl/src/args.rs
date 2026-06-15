use clap::{Args, Parser, Subcommand};

pub const DEFAULT_AGENT_ID: &str = "fae-assistant";
pub const DEFAULT_USER_ID: &str = "master";

#[derive(Args, Debug)]
pub struct AgentArgs {
    #[arg(alias = "name", short, long, help = format!("agent id, default is {}", DEFAULT_AGENT_ID))]
    pub id: Option<String>,
    #[arg(short, long, help = format!("user id, default is {}", DEFAULT_USER_ID))]
    pub user: Option<String>,
    #[arg(short, long, default_value_t = false, help = "start chat with agent")]
    pub chat: bool,
    #[arg(alias = "hs", long, help = "show chat history")]
    pub history: bool,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
    /// init workspace agent and default config ...
    Init,
    /// manage agent
    #[command(alias = "a")]
    Agent(AgentArgs),
    /// uninstall agent
    Uninstall,
}

#[derive(Parser, Debug)]
#[command(name = "fae")]
#[command(version = "0.1.0")]
#[command(about = "Free Agent Engine Control Line Tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
    //指定操作空间
    #[arg(long, default_value = "main", help = "workspace")]
    pub ws: String,
}
