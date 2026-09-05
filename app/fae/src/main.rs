mod app;
mod args;
mod tui;

#[tokio::main]
async fn main() {
    if let Err(error) = app::run(args::parse()).await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
