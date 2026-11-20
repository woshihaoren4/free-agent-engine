use super::{
    store::Store,
    whitelist::{Policy, Update},
};
use crate::server::lifecycle::{self, FileLock, atomic_write};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
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
    whitelist: Policy,
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
                BufReader::new((&mut stream).take(65537)).read_line(&mut line),
            )
            .await;
            if !matches!(read, Ok(Ok(size)) if size > 0 && size <= 65536) || !line.ends_with('\n') {
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
            if request["command"] == "whitelist" {
                let result = serde_json::from_value::<Update>(request["update"].clone())
                    .map_err(anyhow::Error::from)
                    .and_then(|update| whitelist.update(&home, &update));
                let body = match result {
                    Ok(users) => serde_json::json!({
                        "runtime": runtime,
                        "user_id_whitelist": users,
                        "gateway_running": true
                    }),
                    Err(error) => serde_json::json!({
                        "runtime": runtime, "error": error.to_string()
                    }),
                };
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    stream.write_all(format!("{body}\n").as_bytes()),
                )
                .await;
                continue;
            }
            let service_ready = matches!(crate::app::daemon::discover(&home).await, Ok(Some(_)));
            runtime.ready &= service_ready;
            let records = store.pending();
            let body = serde_json::json!({
                "runtime":runtime, "queued":records.iter().filter(|r| r.state == super::store::State::Received).count(),
                "service_ready":service_ready,
                "active":records.iter().filter(|r| matches!(r.state, super::store::State::Running | super::store::State::Submitting)).count(),
                "undelivered":records.iter().filter(|r| r.outcome.is_some()).count(),
                "last_error":records.iter().rev().find_map(|r| r.last_error.clone()),
                "user_id_whitelist":whitelist.snapshot()
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
    query_request(home, command, None).await
}
async fn query_request(
    home: &Path,
    command: &str,
    update: Option<&Update>,
) -> anyhow::Result<Option<serde_json::Value>> {
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
    let request = format!(
        "{}\n",
        serde_json::json!({
            "token":token, "instance":runtime.instance, "command":command, "update":update
        })
    );
    anyhow::ensure!(request.len() <= 65536, "Gateway control request too large");
    let response = tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = TcpStream::connect(("127.0.0.1", runtime.port)).await?;
        stream.write_all(request.as_bytes()).await?;
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
pub async fn update_whitelist(home: &Path, update: Update) -> anyhow::Result<serde_json::Value> {
    prepare(home)?;
    let _start = FileLock::acquire(home.join("lark/lark.lock")).await?;
    if let Some(response) = query_request(home, "whitelist", Some(&update)).await? {
        if let Some(error) = response["error"].as_str() {
            anyhow::bail!("{error}");
        }
        anyhow::ensure!(
            response.get("user_id_whitelist").is_some() && response["gateway_running"] == true,
            "Running gateway does not support whitelist updates; upgrade and restart it once"
        );
        return Ok(response);
    }
    let users = super::whitelist::update_config(home, &update)?;
    Ok(serde_json::json!({"user_id_whitelist":users, "gateway_running":false}))
}
pub async fn start(home: &Path) -> anyhow::Result<serde_json::Value> {
    prepare(home)?;
    let _start = FileLock::acquire(home.join("lark/lark.lock")).await?;
    start_locked(home).await
}
/// Caller must hold lark/lark.lock.
pub(crate) async fn start_locked(home: &Path) -> anyhow::Result<serde_json::Value> {
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
    let _start = FileLock::acquire(home.join("lark/lark.lock")).await?;
    stop_locked(home).await
}
/// Caller must hold lark/lark.lock.
pub(crate) async fn stop_locked(home: &Path) -> anyhow::Result<bool> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn old_gateway_response_does_not_report_success_or_change_config() {
        let home = std::env::temp_dir().join(format!("fae-old-control-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        prepare(&home).unwrap();
        let original = br#"{"user_id_whitelist":["ou_alice"]}"#;
        std::fs::write(home.join("lark.config"), original).unwrap();
        let _gateway_lock = FileLock::try_acquire(&home.join("lark/gateway.lock"))
            .unwrap()
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut runtime = Runtime::new();
        runtime.port = listener.local_addr().unwrap().port();
        atomic_write(
            &home.join("lark/runtime.json"),
            &serde_json::to_vec(&runtime).unwrap(),
            true,
        )
        .unwrap();
        atomic_write(&home.join("lark/control.token"), b"test-token", true).unwrap();
        let old_gateway = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut line = String::new();
            BufReader::new(&mut stream)
                .read_line(&mut line)
                .await
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["command"], "whitelist");
            assert_eq!(request["token"], "test-token");
            stream
                .write_all(format!("{}\n", serde_json::json!({"runtime":runtime})).as_bytes())
                .await
                .unwrap();
        });
        let error = update_whitelist(&home, Update::Add(vec!["ou_bob".into()]))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not support whitelist updates")
        );
        assert_eq!(std::fs::read(home.join("lark.config")).unwrap(), original);
        old_gateway.await.unwrap();
        drop(_gateway_lock);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn whitelist_control_updates_live_policy_and_offline_config() {
        let home = std::env::temp_dir().join(format!("fae-control-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        prepare(&home).unwrap();
        let config = serde_json::json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant",
            "agent_id":"fae","workspace":home,"user_id_whitelist":[]
        });
        atomic_write(
            &home.join("lark.config"),
            &serde_json::to_vec(&config).unwrap(),
            true,
        )
        .unwrap();
        let gateway_lock = FileLock::try_acquire(&home.join("lark/gateway.lock"))
            .unwrap()
            .unwrap();
        let policy = Policy::new(Some(vec![]));
        let mut runtime = Runtime::new();
        let (port, mut stopped) = control_server(
            &home,
            &runtime,
            Arc::new(Store::open(&home, 7).unwrap()),
            policy.clone(),
        )
        .await
        .unwrap();
        runtime.port = port;
        atomic_write(
            &home.join("lark/runtime.json"),
            &serde_json::to_vec(&runtime).unwrap(),
            true,
        )
        .unwrap();
        // Exceed the former 1 KiB control limit.
        let users: Vec<_> = (0..100).map(|id| format!("ou_user_{id:04}")).collect();
        let response = update_whitelist(&home, Update::Add(users.clone()))
            .await
            .unwrap();
        assert_eq!(response["gateway_running"], true);
        assert_eq!(response["runtime"]["instance"], runtime.instance);
        assert_eq!(response["runtime"]["pid"], runtime.pid);
        assert_eq!(policy.snapshot(), Some(users));
        assert!(
            update_whitelist(&home, Update::Set(vec!["bad_id".into()]))
                .await
                .is_err()
        );
        assert!(policy.allows_user("ou_user_0000"));
        update_whitelist(&home, Update::Remove(vec!["ou_user_0000".into()]))
            .await
            .unwrap();
        assert!(!policy.allows_user("ou_user_0000"));
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join("lark.config")).unwrap()).unwrap();
        config["user_id_whitelist"] = serde_json::Value::Null;
        atomic_write(
            &home.join("lark.config"),
            &serde_json::to_vec(&config).unwrap(),
            true,
        )
        .unwrap();
        update_whitelist(&home, Update::Reload).await.unwrap();
        assert!(policy.allows_user("ou_other"));
        assert!(status(&home).await.unwrap().unwrap()["user_id_whitelist"].is_null());
        query(&home, "stop").await.unwrap();
        stopped.recv().await.unwrap();
        drop(gateway_lock);
        std::fs::remove_file(home.join("lark/runtime.json")).unwrap();
        let response = update_whitelist(&home, Update::Set(vec![])).await.unwrap();
        assert_eq!(response["gateway_running"], false);
        let config = super::super::config::Config::load(&home).unwrap();
        assert_eq!(config.user_id_whitelist, Some(vec![]));
        std::fs::remove_dir_all(home).unwrap();
    }
}
