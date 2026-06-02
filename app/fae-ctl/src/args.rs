use clap::{Parser, Subcommand, Args};

#[derive(Args, Debug)]
pub struct AgentArgs {
    #[arg(alias = "name", short, long, help = "agent id, default is main")]
    pub id: Option<String>,
    #[arg(short, long, help = "user id, default master")]
    pub user: Option<String>,
    #[arg(short, long, help = "start chat with agent")]
    pub chat: Option<String>,
    #[arg(alias = "hs",long, help = "show chat history")]
    pub history: bool,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
    /// init workspace agent and default config ...
    Init,
    /// manage agent
    #[command(alias = "a")]
    Agent(AgentArgs),
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