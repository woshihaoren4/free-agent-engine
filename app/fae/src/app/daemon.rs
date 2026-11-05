use super::client;
use crate::{
    protocol::*,
    server::lifecycle::{self, FileLock},
};
use anyhow::Context;
pub use lifecycle::read_config;
use std::{
    io::{Read, Seek, SeekFrom},
    net::{IpAddr, Ipv4Addr},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub async fn discover(home: &Path) -> anyhow::Result<Option<ServerConfig>> {
    let config = match read_config(home) {
        Ok(config) => config,
        Err(error) => {
            if is_permission_denied(&error) {
                return Err(error);
            }
            if FileLock::try_acquire(&home.join("server.lock"))?.is_none() {
                anyhow::bail!(
                    "Service holds server.lock but discovery information is missing or invalid: {error}"
                );
            }
            return Ok(None);
        }
    };
    if lifecycle::process_alive(config.pid)? {
        match client::hello(home, &config).await {
            Ok(_) => return Ok(Some(config)),
            Err(error) => {
                if is_permission_denied(&error) {
                    return Err(error);
                }
                if FileLock::try_acquire(&home.join("server.lock"))?.is_none() {
                    anyhow::bail!(
                        "Service is unavailable at {}:{}: {error:#}",
                        config.address,
                        config.port
                    );
                }
            }
        }
    } else if FileLock::try_acquire(&home.join("server.lock"))?.is_none() {
        anyhow::bail!("Service lock is held despite stale discovery information");
    }
    Ok(None)
}

fn is_permission_denied(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
}

pub async fn ensure(home: &Path) -> anyhow::Result<ServerConfig> {
    start(home, IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT, false).await
}

fn startup_diagnostic(home: &Path, offset: u64) -> String {
    let read = || -> std::io::Result<String> {
        let mut log = std::fs::File::open(home.join("log/server.log"))?;
        log.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        log.take(8 * 1024).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
    };
    match read() {
        Ok(text) if !text.is_empty() => format!("\n{text}"),
        _ => String::new(),
    }
}

pub async fn start(
    home: &Path,
    address: IpAddr,
    port: u16,
    explicit: bool,
) -> anyhow::Result<ServerConfig> {
    lifecycle::check_home_owner(home)?;
    let _start = FileLock::acquire(home.join("start.lock")).await?;
    if let Some(config) = discover(home).await? {
        if explicit {
            anyhow::ensure!(
                config.address == address && config.port == port,
                "Service already runs at {}:{}; stop it before changing address/port",
                config.address,
                config.port
            );
        }
        return Ok(config);
    }
    {
        let _probe = FileLock::try_acquire(&home.join("server.lock"))?.ok_or_else(|| {
            anyhow::anyhow!("Service lock is held; cannot start another instance")
        })?;
        lifecycle::clear_discovery(home)?;
    }
    let log = lifecycle::log_file(home)?;
    let log_offset = log.metadata()?.len();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--fae-home")
        .arg(home)
        .args(["server", "start", "--address"])
        .arg(address.to_string())
        .args(["--port", &port.to_string(), "--foreground"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200);
    }
    let mut child = command.spawn().context("spawn FAE service")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut delay = 25;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!(
                "Service exited {status}; address {address}:{port}; see {}{}",
                home.join("log/server.log").display(),
                startup_diagnostic(home, log_offset)
            );
        }
        if let Ok(config) = read_config(home) {
            if config.pid == child.id() && client::hello(home, &config).await.is_ok() {
                // Reap the child eventually without tying its lifetime to the client.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(config);
            }
        }
        tokio::time::sleep(Duration::from_millis(delay)).await;
        delay = (delay * 2).min(500);
    }
    anyhow::bail!(
        "Service readiness timed out; see {}{}",
        home.join("log/server.log").display(),
        startup_diagnostic(home, log_offset)
    )
}

pub async fn stop(home: &Path) -> anyhow::Result<bool> {
    let _start = FileLock::acquire(home.join("start.lock")).await?;
    let Some(config) = discover(home).await? else {
        if let Some(_probe) = FileLock::try_acquire(&home.join("server.lock"))? {
            lifecycle::clear_discovery(home)?;
        }
        return Ok(false);
    };
    client::control(home, &config, ControlMessage::Shutdown).await?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(_probe) = FileLock::try_acquire(&home.join("server.lock"))? {
            anyhow::ensure!(
                !home.join("server.config").exists(),
                "Service exited without completing cleanup; see server.log"
            );
            if !lifecycle::process_alive(config.pid)? {
                return Ok(true);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!(
        "Service shutdown exceeded 10 seconds; see {}",
        home.join("log/server.log").display()
    )
}
