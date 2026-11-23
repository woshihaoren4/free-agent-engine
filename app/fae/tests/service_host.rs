use fae::{app::client, protocol::*};
use fae_agent::protocol::VERSION;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU16, Ordering},
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::TcpListener};

struct Service {
    home: PathBuf,
    child: Child,
    config: ServerConfig,
}
impl Service {
    async fn start(model_base: Option<&str>) -> Self {
        for attempt in 0..5 {
            let home =
                std::env::temp_dir().join(format!("fae-service-test-{}", wd_tools::uuid::v4()));
            std::fs::create_dir_all(&home).unwrap();
            let home = home.canonicalize().unwrap();
            // Releasing an OS-selected ephemeral port lets another test's outgoing
            // connection claim it before the daemon binds. Keep service fixtures
            // outside the ephemeral range and give each fixture a distinct port.
            static NEXT_PORT: AtomicU16 = AtomicU16::new(20_000);
            let listener = loop {
                let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
                assert!(port < 30_000, "service fixture port range exhausted");
                match TcpListener::bind(("127.0.0.1", port)).await {
                    Ok(listener) => break listener,
                    Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                    Err(error) => panic!("reserve service fixture port: {error}"),
                }
            };
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let mut command = Command::new(env!("CARGO_BIN_EXE_fae"));
            command
                .arg("--fae-home")
                .arg(&home)
                .args(["server", "start", "--foreground", "--port"])
                .arg(port.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::from(
                    std::fs::File::create(home.join("startup.log")).unwrap(),
                ));
            if let Some(base) = model_base {
                command
                    .env("OPENAI_BASE_URL", base)
                    .env("OPENAI_API_KEY", "test-key");
            }
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                // A daemon may be launched by a host that blocks process signals.
                // Exercise restoration before Tokio workers are constructed.
                unsafe {
                    command.pre_exec(|| {
                        let mut signals = std::mem::zeroed();
                        libc::sigemptyset(&mut signals);
                        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGCHLD] {
                            libc::sigaddset(&mut signals, signal);
                        }
                        let result =
                            libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut());
                        if result != 0 {
                            return Err(std::io::Error::from_raw_os_error(result));
                        }
                        Ok(())
                    });
                }
            }
            let child = command.spawn().unwrap();
            let placeholder = ServerConfig {
                instance_id: "".into(),
                pid: child.id(),
                address: "127.0.0.1".parse().unwrap(),
                port,
                protocol_version: VERSION,
                app_version: "".into(),
                started_at: "".into(),
                executable: "fae".into(),
            };
            let mut service = Self {
                home,
                child,
                config: placeholder,
            };
            let ready = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if service.child.try_wait().unwrap().is_some() {
                        return false;
                    }
                    if let Ok(config) = fae::server::lifecycle::read_config(&service.home) {
                        if client::hello(&service.home, &config).await.is_ok() {
                            service.config = config;
                            return true;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            if ready {
                return service;
            }
            let diagnostic = std::fs::read_to_string(service.home.join("startup.log")).unwrap();
            // Concurrent fork/exec can briefly inherit a fixture's reservation
            // socket. Retry only this bind race, never execution or protocol errors.
            assert!(
                attempt < 4 && diagnostic.contains("address_in_use"),
                "service exited before readiness: {diagnostic}"
            );
        }
        unreachable!()
    }
    async fn init(&self, id: &str) {
        client::control(
            &self.home,
            &self.config,
            fae_agent::protocol::RequestPayload::EngineInitialize(
                fae_agent::protocol::InitializeRequest {
                    agent_id: id.into(),
                    model: "mock".into(),
                    force: false,
                },
            ),
        )
        .await
        .unwrap();
        std::fs::write(
            self.home.join(format!("agents/{id}_prompt.txt")),
            "You are a test agent.",
        )
        .unwrap();
    }
    async fn stop(&mut self) {
        assert!(fae::app::daemon::stop(&self.home).await.unwrap());
        assert!(self.child.wait().unwrap().success());
        assert!(!self.home.join("server.config").exists());
        assert!(!self.home.join("lark/lark.token").exists());
        assert!(!fae::app::daemon::stop(&self.home).await.unwrap());
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

struct DetachedService(PathBuf);
impl Drop for DetachedService {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_fae"))
            .arg("--fae-home")
            .arg(&self.0)
            .args(["server", "stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_resume_validates_target_and_commits_idle_workspace() {
    use fae_agent::{PlanTarget, protocol::*};
    let mut service = Service::start(None).await;
    service.init("alpha").await;
    service.init("beta").await;
    let first_workspace = service.home.join("first");
    let second_workspace = service.home.join("second");
    std::fs::create_dir(&first_workspace).unwrap();
    std::fs::create_dir(&second_workspace).unwrap();
    let create = SessionCreateRequest {
        name: "alpha".into(),
        target: PlanTarget::Agent {
            agent_id: "alpha".into(),
        },
        workspace: Some(first_workspace.to_string_lossy().into_owned()),
        options: Default::default(),
        channels: vec![],
    };
    let mut original = client::Client::open(
        &service.home,
        &service.config,
        create.clone(),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let id = original.sid().to_owned();
    let mut other_target = create.clone();
    other_target.target = PlanTarget::Agent {
        agent_id: "beta".into(),
    };
    let conflict = client::Client::open(
        &service.home,
        &service.config,
        other_target,
        Some(id.clone()),
        None,
        None,
    )
    .await;
    assert!(
        conflict
            .err()
            .unwrap()
            .to_string()
            .contains("session_conflict")
    );
    let mut update = create;
    update.workspace = Some(second_workspace.to_string_lossy().into_owned());
    let mut resumed =
        client::Client::open(&service.home, &service.config, update, Some(id), None, None)
            .await
            .unwrap();
    assert_eq!(
        resumed.info.session_config.workspace.root,
        second_workspace.to_string_lossy()
    );
    let ResponsePayload::SessionGet(fact) = original
        .request(RequestPayload::SessionGet(Empty {}))
        .await
        .unwrap()
    else {
        panic!("expected summary");
    };
    assert_eq!(
        fact.session_config.workspace.root,
        resumed.info.session_config.workspace.root
    );
    let stored = service
        .home
        .join("session/alpha/master")
        .join(format!("{}.json", original.sid()));
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(stored).unwrap()).unwrap(),
        json!(second_workspace.to_string_lossy()),
    );
    original.close().await.unwrap();
    resumed.close().await.unwrap();
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_preserves_custom_endpoint_without_lark_config() {
    let mut service = Service::start(None).await;
    let _cleanup = DetachedService(service.home.clone());
    let previous = service.config.clone();
    let output = Command::new(env!("CARGO_BIN_EXE_fae"))
        .arg("--fae-home")
        .arg(&service.home)
        .arg("restart")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("no lark.config"));
    assert!(service.child.wait().unwrap().success());
    let config = fae::server::lifecycle::read_config(&service.home).unwrap();
    assert_ne!(config.pid, previous.pid);
    assert_ne!(config.instance_id, previous.instance_id);
    assert_eq!(config.address, previous.address);
    assert_eq!(config.port, previous.port);
    client::hello(&service.home, &config).await.unwrap();
    assert!(!service.home.join("lark/runtime.json").exists());
    assert!(fae::app::daemon::stop(&service.home).await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_rejects_invalid_lark_config_before_stopping_service() {
    let mut service = Service::start(None).await;
    for config in [
        json!({}),
        json!({
            "version":1,"app_id":"test-app","app_secret":"test-secret",
            "tenant_key":"tenant","agent_id":"missing","workspace":service.home
        }),
    ] {
        std::fs::write(
            service.home.join("lark.config"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_fae"))
            .arg("--fae-home")
            .arg(&service.home)
            .arg("restart")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("before restart"));
        assert_eq!(
            fae::server::lifecycle::read_config(&service.home)
                .unwrap()
                .instance_id,
            service.config.instance_id
        );
        client::hello(&service.home, &service.config).await.unwrap();
    }
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_stops_gateway_first_and_reports_gateway_start_failure() {
    use fae::server::lifecycle::FileLock;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut service = Service::start(None).await;
    let _cleanup = DetachedService(service.home.clone());
    service.init("fae").await;
    std::fs::write(
        service.home.join("lark.config"),
        serde_json::to_vec(&json!({
            "version":1,"app_id":"test-app","app_secret":"test-secret",
            "tenant_key":"tenant","agent_id":"fae","workspace":service.home
        }))
        .unwrap(),
    )
    .unwrap();
    // A local control endpoint exercises gateway shutdown without contacting
    // Feishu. Removing the config during shutdown makes the next start fail.
    let lock = FileLock::try_acquire(&service.home.join("lark/gateway.lock"))
        .unwrap()
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut runtime = fae::lark::lifecycle::Runtime::new();
    runtime.port = listener.local_addr().unwrap().port();
    runtime.ready = true;
    std::fs::write(
        service.home.join("lark/runtime.json"),
        serde_json::to_vec(&runtime).unwrap(),
    )
    .unwrap();
    std::fs::write(service.home.join("lark/control.token"), "test-token").unwrap();
    let home = service.home.clone();
    let previous = service.config.clone();
    let gateway = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut line = String::new();
        BufReader::new(&mut stream)
            .read_line(&mut line)
            .await
            .unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["command"], "stop");
        assert_eq!(request["token"], "test-token");
        assert_eq!(request["instance"], runtime.instance);
        // The original service must still be available when the gateway stops.
        client::hello(&home, &previous).await.unwrap();
        stream
            .write_all(format!("{}\n", json!({"runtime":runtime})).as_bytes())
            .await
            .unwrap();
        std::fs::remove_file(home.join("lark/runtime.json")).unwrap();
        std::fs::remove_file(home.join("lark.config")).unwrap();
        drop(lock);
    });
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_fae"))
        .arg("--fae-home")
        .arg(&service.home)
        .arg("restart")
        .output()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), gateway)
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("service restarted, but Lark gateway failed to start")
    );
    assert!(service.child.wait().unwrap().success());
    let config = fae::server::lifecycle::read_config(&service.home).unwrap();
    assert_ne!(config.instance_id, service.config.instance_id);
    assert_eq!(config.port, service.config.port);
    client::hello(&service.home, &config).await.unwrap();
    assert!(fae::app::daemon::stop(&service.home).await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg(unix)]
async fn unreadable_credentials_and_discovery_preserve_running_service() {
    use std::os::unix::fs::PermissionsExt;
    // Root bypasses mode-based read restrictions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let mut service = Service::start(None).await;
    for name in ["server/server.token", "server.config"] {
        let path = service.home.join(name);
        let permissions = std::fs::metadata(&path).unwrap().permissions();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0)).unwrap();
        let outputs: Vec<_> = ["show", "status", "stop", "start"]
            .into_iter()
            .map(|command| {
                Command::new(env!("CARGO_BIN_EXE_fae"))
                    .arg("--fae-home")
                    .arg(&service.home)
                    .args(["server", command])
                    .output()
                    .unwrap()
            })
            .collect();
        std::fs::set_permissions(&path, permissions).unwrap();
        for output in outputs {
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success());
            assert!(error.contains(&path.display().to_string()), "{error}");
            assert!(error.contains("Permission denied"), "{error}");
            assert!(error.contains("sudo"), "{error}");
            assert!(!error.contains("Service is unavailable"), "{error}");
        }
        assert_eq!(
            fae::server::lifecycle::read_config(&service.home)
                .unwrap()
                .instance_id,
            service.config.instance_id
        );
        client::hello(&service.home, &service.config).await.unwrap();
    }
    service.stop().await;
}

#[cfg(unix)]
#[test]
fn service_start_rejects_another_users_home_before_creating_runtime_files() {
    use std::os::unix::fs::MetadataExt;
    let home = PathBuf::from("/");
    if std::fs::metadata(&home).unwrap().uid() == unsafe { libc::geteuid() } {
        return;
    }
    for foreground in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fae"));
        command
            .arg("--fae-home")
            .arg(&home)
            .args(["server", "start", "--port", "1666"]);
        if foreground {
            command.arg("--foreground");
        }
        let output = command.output().unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(error.contains("FAE home belongs to UID"), "{error}");
        assert!(error.contains("without sudo"), "{error}");
        assert!(error.contains("--port 6666"), "{error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_reuses_custom_endpoint_and_preserves_locked_corrupt_discovery() {
    let mut service = Service::start(None).await;
    let pid = service.config.pid;
    let args = ["server", "start", "--port"];
    let output = Command::new(env!("CARGO_BIN_EXE_fae"))
        .arg("--fae-home")
        .arg(&service.home)
        .args(args)
        .arg(service.config.port.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fae::app::daemon::ensure(&service.home).await.unwrap().pid,
        pid
    );
    let mismatch = Command::new(env!("CARGO_BIN_EXE_fae"))
        .arg("--fae-home")
        .arg(&service.home)
        .args(["server", "start", "--port", "1"])
        .output()
        .unwrap();
    assert!(!mismatch.status.success());
    std::fs::write(service.home.join("server.config"), "{broken").unwrap();
    assert!(
        fae::app::daemon::ensure(&service.home)
            .await
            .unwrap_err()
            .to_string()
            .contains("discovery")
    );
    fae::server::lifecycle::atomic_write(
        &service.home.join("server.config"),
        &serde_json::to_vec(&service.config).unwrap(),
        false,
    )
    .unwrap();
    service.stop().await;

    std::fs::create_dir_all(service.home.join("server")).unwrap();
    std::fs::write(
        service.home.join("server/server.log"),
        "error: old-startup-failure\n",
    )
    .unwrap();
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = occupied.local_addr().unwrap().port();
    let output = Command::new(env!("CARGO_BIN_EXE_fae"))
        .arg("--fae-home")
        .arg(&service.home)
        .args(["server", "start", "--port"])
        .arg(port.to_string())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!service.home.join("server.config").exists());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("address_in_use"), "{diagnostic}");
    assert!(!diagnostic.contains("old-startup-failure"), "{diagnostic}");
    assert!(
        std::fs::read_to_string(service.home.join("server/server.log"))
            .unwrap()
            .contains("address_in_use")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_detached_start_has_one_instance_and_survives_clients() {
    let mut service = Service::start(None).await;
    let port = service.config.port;
    service.stop().await;
    struct Detached(PathBuf);
    impl Drop for Detached {
        fn drop(&mut self) {
            let _ = Command::new(env!("CARGO_BIN_EXE_fae"))
                .arg("--fae-home")
                .arg(&self.0)
                .args(["server", "stop"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    let _cleanup = Detached(service.home.clone());
    let start = || {
        Command::new(env!("CARGO_BIN_EXE_fae"))
            .arg("--fae-home")
            .arg(&service.home)
            .args(["server", "start", "--port"])
            .arg(port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = start();
    let second = start();
    for child in [first, second] {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let config = fae::server::lifecycle::read_config(&service.home).unwrap();
    assert_ne!(config.pid, service.child.id());
    assert_eq!(config.port, port);
    client::hello(&service.home, &config).await.unwrap();
    assert!(service.home.join("server/server.lock").is_file());
    assert!(service.home.join("server/start-stop.lock").is_file());
    assert!(service.home.join("server/server.token").is_file());
    assert!(!service.home.join("server.lock").exists());
    assert!(!service.home.join("server.token").exists());
    assert!(!service.home.join("start.lock").exists());
    assert!(service.home.join("schedule/timer.lock").is_file());
    assert!(!service.home.join("timer.lock").exists());
    assert!(
        fae::server::lifecycle::FileLock::try_acquire(&service.home.join("server/server.lock"))
            .unwrap()
            .is_none()
    );
    assert!(
        fae::server::lifecycle::FileLock::try_acquire(&service.home.join("server/start-stop.lock"))
            .unwrap()
            .is_some()
    );
    assert!(fae::app::daemon::stop(&service.home).await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn distinct_runtime_instances_serialize_shared_user_memory_updates() {
    use fae_agent::{UserMemoryCategory, UserMemoryConfidence, UserMemoryResponse};
    let home = std::env::temp_dir().join(format!("fae-shared-memory-{}", wd_tools::uuid::v4()));
    std::fs::create_dir_all(&home).unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..32 {
        let home = home.clone();
        tasks.spawn(async move {
            fae_engine::UserMemoryRuntime::with_host_dir(home)
                .update(
                    "alice",
                    None,
                    UserMemoryCategory::Preference,
                    format!("value-{index}"),
                    UserMemoryConfidence::UserStated,
                )
                .await
                .unwrap()
        });
    }
    let mut ids = std::collections::HashSet::new();
    while let Some(result) = tasks.join_next().await {
        let UserMemoryResponse::Updated { memory, .. } = result.unwrap() else {
            panic!()
        };
        assert!(ids.insert(memory.id));
    }
    let UserMemoryResponse::Memories { memories, .. } =
        fae_engine::UserMemoryRuntime::with_host_dir(&home)
            .query("alice")
            .await
            .unwrap()
    else {
        panic!()
    };
    assert_eq!(memories.len(), 32);
    std::fs::remove_dir_all(home).unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_performs_owned_discovery_cleanup() {
    let mut service = Service::start(None).await;
    assert_eq!(
        unsafe { libc::kill(service.config.pid as i32, libc::SIGTERM) },
        0
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(status) = service.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(!service.home.join("server.config").exists());
    assert!(!service.home.join("server/server.token").exists());
    assert!(
        fae::server::lifecycle::FileLock::try_acquire(&service.home.join("server/server.lock"))
            .unwrap()
            .is_some()
    );
}
