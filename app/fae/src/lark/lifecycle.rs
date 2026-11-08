use super::store::Store;
use crate::server::lifecycle::{self, FileLock, atomic_write};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Runtime {
    pub instance: String,
    pub pid: u32,
    pub port: u16,
    pub ready: bool,
    pub version: u16,
}
impl Runtime {
    pub fn new() -> Self {
        Self {
            instance: wd_tools::uuid::v4(),
            pid: std::process::id(),
            port: 0,
            ready: false,
            version: 1,
        }
    }
}
pub fn prepare(home: &Path) -> anyhow::Result<()> {
    lifecycle::check_home_owner(home)?;
    std::fs::create_dir_all(home.join("lark"))?;
    Ok(())
}
pub async fn control_server(
    home: &Path,
    runtime: &Runtime,
    store: Arc<Store>,
) -> anyhow::Result<(u16, mpsc::Receiver<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let token = wd_tools::uuid::v4();
    atomic_write(&home.join("lark/control.token"), token.as_bytes(), true)?;
    let instance = runtime.instance.clone();
    let home = home.to_path_buf();
    let (stop, stopped) = mpsc::channel(1);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut line = String::new();
            let read = tokio::time::timeout(
                Duration::from_secs(2),
                BufReader::new(&mut stream).read_line(&mut line),
            )
            .await;
            if !matches!(read, Ok(Ok(size)) if size < 1024) {
                continue;
            }
            let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if request["token"].as_str() != Some(&token)
                || request["instance"].as_str() != Some(&instance)
            {
                continue;
            }
            let Ok(runtime) = std::fs::read(home.join("lark/runtime.json")) else {
                continue;
            };
            let Ok(mut runtime) = serde_json::from_slice::<Runtime>(&runtime) else {
                continue;
            };
            let service_ready = matches!(crate::app::daemon::discover(&home).await, Ok(Some(_)));
            runtime.ready &= service_ready;
            let records = store.pending();
            let body = serde_json::json!({
                "runtime":runtime, "queued":records.iter().filter(|r| r.state == super::store::State::Received).count(),
                "service_ready":service_ready,
                "active":records.iter().filter(|r| matches!(r.state, super::store::State::Running | super::store::State::Submitting)).count(),
                "undelivered":records.iter().filter(|r| r.outcome.is_some()).count(),
                "last_error":records.iter().rev().find_map(|r| r.last_error.clone())
            });
            let text = format!("{body}\n");
            let _ = tokio::time::timeout(Duration::from_secs(2), stream.write_all(text.as_bytes()))
                .await;
            if request["command"] == "stop" {
                let _ = stop.try_send(());
                break;
            }
        }
    });
    Ok((port, stopped))
}
async fn query(home: &Path, command: &str) -> anyhow::Result<Option<serde_json::Value>> {
    if !home.join("lark/runtime.json").exists() {
        if home.join("lark").exists()
            && FileLock::try_acquire(&home.join("lark/gateway.lock"))?.is_none()
        {
            anyhow::bail!(
                "Gateway lock held without valid runtime information; inspect gateway.log"
            );
        }
        return Ok(None);
    }
    let runtime: Runtime = serde_json::from_slice(&std::fs::read(home.join("lark/runtime.json"))?)?;
    anyhow::ensure!(
        runtime.pid > 0 && runtime.pid <= i32::MAX as u32 && runtime.port > 0,
        "Invalid gateway runtime"
    );
    if !lifecycle::process_alive(runtime.pid)? {
        return Ok(None);
    }
    let token = std::fs::read_to_string(home.join("lark/control.token"))?;
    let response = tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = TcpStream::connect(("127.0.0.1", runtime.port)).await?;
        stream
            .write_all(
                format!(
                    "{}\n",
                    serde_json::json!({
                        "token":token, "instance":runtime.instance, "command":command
                    })
                )
                .as_bytes(),
            )
            .await?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await?;
        let value: serde_json::Value = serde_json::from_str(&line)?;
        anyhow::ensure!(
            value["runtime"]["instance"].as_str() == Some(&runtime.instance)
                && value["runtime"]["pid"].as_u64() == Some(runtime.pid as u64),
            "Gateway instance mismatch"
        );
        Ok::<_, anyhow::Error>(value)
    })
    .await?;
    match response {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            if FileLock::try_acquire(&home.join("lark/gateway.lock"))?.is_some() {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}
pub async fn status(home: &Path) -> anyhow::Result<Option<serde_json::Value>> {
    query(home, "status").await
}
pub async fn start(home: &Path) -> anyhow::Result<serde_json::Value> {
    prepare(home)?;
    let _start = FileLock::acquire(home.join("lark/start.lock")).await?;
    if let Some(status) = status(home).await? {
        return Ok(status);
    }
    let config = super::config::Config::load(home)?;
    config.check_agent(home).await?;
    let log_path = home.join("lark/gateway.log");
    if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > 8 * 1024 * 1024) {
        std::fs::rename(&log_path, home.join("lark/gateway.log.1"))?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--fae-home")
        .arg(home)
        .args(["lark", "start", "--foreground"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200);
    }
    let mut child = command.spawn()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(exit) = child.try_wait()? {
            anyhow::bail!("Gateway exited {exit}; see lark/gateway.log");
        }
        if let Ok(Some(status)) = status(home).await {
            if status["runtime"]["pid"].as_u64() == Some(child.id() as u64)
                && status["runtime"]["ready"] == true
            {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(status);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Gateway readiness timed out; see lark/gateway.log");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
pub async fn stop(home: &Path) -> anyhow::Result<bool> {
    if !home.join("lark").exists() {
        return Ok(false);
    }
    let _start = FileLock::acquire(home.join("lark/start.lock")).await?;
    if query(home, "stop").await?.is_none() {
        return Ok(false);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        if FileLock::try_acquire(&home.join("lark/gateway.lock"))?.is_some() {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("Gateway shutdown timed out; inspect lark/gateway.log");
}
