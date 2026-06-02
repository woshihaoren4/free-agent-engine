use clap::Parser;

mod args;
mod init_project;
mod agents;
mod chat_ui;

#[tokio::main]
async fn main() {
    let cli = args::Cli::parse();
    let ws = cli.ws;
    match cli.command {
        args::Commands::Init => {
            init_project::InitProject::init(ws).await;
        }
        args::Commands::Agent(args) => {
            agents::Agents::exec(ws, args).await;
        }
    }
}
