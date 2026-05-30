use clap::Parser;

mod args;
mod init_project;
mod agents;
mod chat_ui;

#[tokio::main]
async fn main() {
    let cli = args::Cli::parse();
    let wd = cli.ws;
    match cli.command {
        args::Commands::Init => {
            init_project::InitProject::init(wd).await;
        }
        args::Commands::Agent(args) => {
            agents::Agents::exec(wd,args).await;
        }
    }
}
