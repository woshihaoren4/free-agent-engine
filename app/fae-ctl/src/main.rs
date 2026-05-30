use clap::Parser;

mod args;
mod init_project;

fn main() {
    let cli = args::Cli::parse();
    let wd = cli.ws;
    match cli.command {
        args::Commands::Init => {
            init_project::InitProject::init(wd);
        }
        args::Commands::Agent(args) => {
            println!("{:#?}", args);
        }
    }
}
