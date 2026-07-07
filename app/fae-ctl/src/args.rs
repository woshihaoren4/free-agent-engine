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
    #[arg(alias = "ss", long, help = "show session history")]
    pub session_history: bool,
    #[arg(short, long,help="session id, default is main_session_id_1")]
    pub session_id: Option<String>,
    #[arg(alias = "new", long, help = "create new session")]
    pub new_session: bool,
}

impl Default for AgentArgs {
    fn default() -> Self {
        Self {
            id: Some(DEFAULT_AGENT_ID.to_string()),
            user: Some(DEFAULT_USER_ID.to_string()),
            chat: true,
            history: false,
            session_history: false,
            new_session: false,
            session_id: None,
        }
    }
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
    pub command: Option<Commands>,
    //指定操作空间
    #[arg(long, default_value = "main", help = "workspace")]
    pub ws: String,
}
