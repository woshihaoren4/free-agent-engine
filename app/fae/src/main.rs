use fae::app;

fn main() {
    let result = (|| {
        fae::server::lifecycle::prepare_process_signals()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(app::run(app::args::parse()))
    })();
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
