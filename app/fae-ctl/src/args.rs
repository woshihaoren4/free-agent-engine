use clap::{Parser, Subcommand, Args};

#[derive(Args, Debug)]
struct InitArgs {
    /// 项目名称，可选
    pub name: Option<String>,
    /// 强制初始化
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct AgentArgs {
    #[arg(short, long, default_value_t = false, help = "show all agents")]
    pub list: bool,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
    /// 初始化项目
    Init,
    /// 管理智能体
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