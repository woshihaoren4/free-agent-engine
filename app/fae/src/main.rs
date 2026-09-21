mod app;
mod args;
mod init;
mod tui;
mod workspace;

#[tokio::main]
async fn main() {
    if let Err(error) = app::run(args::parse()).await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
