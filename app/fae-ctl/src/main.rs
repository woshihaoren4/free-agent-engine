use clap::Parser;

mod agents;
mod args;
mod chat_ui;
mod init_project;
mod uninstall;

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
        args::Commands::Uninstall => {
            uninstall::Uninstall {}.exec();
        }
    }
}
