use super::daemon;
use crate::{
    lark::{config::Config, lifecycle as lark},
    server::lifecycle::{self, FileLock},
};
use anyhow::Context;
use std::path::Path;

pub async fn run(home: &Path) -> anyhow::Result<()> {
    lifecycle::check_home_owner(home)?;
    // Block gateway starts (which may implicitly start the service) until both
    // components have completed their restart.
    let _gateway = FileLock::acquire(home.join("lark/lark.lock")).await?;
    let restart_gateway =
        home.join("lark.config").try_exists()? || lark::status(home).await?.is_some();
    if restart_gateway {
        let config = Config::load(home).context("validate Lark configuration before restart")?;
        config
            .check_agent(home)
            .await
            .context("validate Lark agent before restart")?;
    }

    if lark::stop_locked(home)
        .await
        .context("stop Lark gateway before restarting service")?
    {
        println!("Lark gateway stopped");
    }
    let config = daemon::restart(home).await?;
    println!(
        "Service running: PID {} {}:{}\nConfig: {}",
        config.pid,
        config.address,
        config.port,
        home.join("server.config").display()
    );
    if restart_gateway {
        let status = lark::start_locked(home)
            .await
            .context("service restarted, but Lark gateway failed to start")?;
        println!("Lark gateway running: {status}");
    } else {
        println!("Lark gateway skipped: no lark.config");
    }
    Ok(())
}
