use serde_json::Value;
use std::{path::PathBuf, process::Output, time::Duration};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fae-plugins-test-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    async fn run(&self, arguments: &[&str]) -> Output {
        tokio::time::timeout(
            Duration::from_secs(20),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_fae"))
                .arg("--fae-home")
                .arg(self.0.join("home"))
                .args(arguments)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn plugin_cli_installs_shows_overwrites_and_uninstalls_without_starting_service() {
    let fixture = Fixture::new();
    let source = fixture.0.join("demo.py");
    std::fs::write(&source, "raise RuntimeError('show must not import')").unwrap();
    let path = source.to_str().unwrap();
    let installed = fixture.run(&["plugin", "install", path]).await;
    assert!(installed.status.success(), "{installed:?}");
    let shown = fixture.run(&["plugin", "show", "demo"]).await;
    assert!(shown.status.success(), "{shown:?}");
    let details: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(details["demo"]["installed"], true);
    assert_eq!(details["demo"]["host"], "offline");
    let shown = fixture.run(&["plugin", "show", "*"]).await;
    assert!(shown.status.success(), "{shown:?}");
    let text = String::from_utf8(shown.stdout).unwrap();
    assert!(text.contains("ORDER\tID\tINSTALLED\tENABLED\tLOADED"));
    assert!(text.contains("1\tdemo\ttrue\ttrue\tfalse"));
    let default_shown = fixture.run(&["plugin", "show"]).await;
    assert!(default_shown.status.success(), "{default_shown:?}");
    assert_eq!(String::from_utf8(default_shown.stdout).unwrap(), text);
    std::fs::write(&source, "replacement").unwrap();
    assert!(
        fixture
            .run(&["plugin", "install", path])
            .await
            .status
            .success()
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("home/plugins/demo.py")).unwrap(),
        "replacement"
    );
    let removed = fixture.run(&["plugin", "uninstall", "demo"]).await;
    assert!(removed.status.success(), "{removed:?}");
    assert!(fixture.0.join("home/plugins/demo.py").exists());
    let config: Value = serde_json::from_slice(
        &std::fs::read(fixture.0.join("home/plugins/plugins.config")).unwrap(),
    )
    .unwrap();
    assert_eq!(config["plugins"][0]["installed"], false);
    assert_eq!(config["plugins"][0]["enabled"], false);
    assert!(!fixture.0.join("home/server.config").exists());
    assert!(!fixture.0.join("home/plugin-state").exists());
    assert!(
        !fixture
            .run(&["plugin", "--show", "demo"])
            .await
            .status
            .success()
    );
    assert!(!fixture.run(&["plugin", "demo"]).await.status.success());
}

#[tokio::test]
async fn plugin_cli_updates_running_service_without_restart() {
    let fixture = Fixture::new();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);
    let started = fixture.run(&["server", "start", "--port", &port]).await;
    assert!(started.status.success(), "{started:?}");
    let server_path = fixture.0.join("home/server.config");
    let server_before = std::fs::read(&server_path).unwrap();
    // Always shut down the detached service, even if an assertion fails.
    struct StopService(PathBuf);
    impl Drop for StopService {
        fn drop(&mut self) {
            let _ = std::process::Command::new(env!("CARGO_BIN_EXE_fae"))
                .arg("--fae-home")
                .arg(&self.0)
                .args(["server", "stop"])
                .output();
        }
    }
    let _stop = StopService(fixture.0.join("home"));
    for (id, mode) in [
        ("direct", "direct"),
        ("first", "chain"),
        ("second", "chain"),
    ] {
        let source = fixture.0.join(format!("{id}.py"));
        let base = if mode == "chain" {
            "ChainSingleAgentHook"
        } else {
            "SingleAgentHook"
        };
        std::fs::write(
            &source,
            format!(
                "from fae_plugin import {base}\n\
             def plugin_config():\n    return {{'plugin_version':'1.0.0',\
             'description':'{id}','registration_mode':'{mode}',\
             'mounts':[{{'point':'SingleAgentHook','implementation':'Demo'}}]}}\n\
             class Demo({base}):\n    pass\n"
            ),
        )
        .unwrap();
        let output = fixture
            .run(&["plugin", "install", source.to_str().unwrap()])
            .await;
        assert!(output.status.success(), "{output:?}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Restart"));
        let shown = fixture.run(&["plugin", "show", id]).await;
        assert!(shown.status.success(), "{shown:?}");
        let value: Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(value[id]["loaded"], true);
        assert_eq!(value[id]["host"], "online");
    }
    let details = fixture.run(&["plugin", "show", "direct"]).await;
    let direct: Value = serde_json::from_slice(&details.stdout).unwrap();
    let chain = fixture.run(&["plugin", "show", "second"]).await;
    let chain: Value = serde_json::from_slice(&chain.stdout).unwrap();
    assert_eq!(
        chain["second"]["chain_order"],
        serde_json::json!(["first", "second"])
    );
    let replacement = fixture
        .run(&[
            "plugin",
            "install",
            fixture.0.join("direct.py").to_str().unwrap(),
        ])
        .await;
    assert!(replacement.status.success(), "{replacement:?}");
    let details = fixture.run(&["plugin", "show", "direct"]).await;
    let replaced: Value = serde_json::from_slice(&details.stdout).unwrap();
    assert_ne!(direct["direct"]["load_id"], replaced["direct"]["load_id"]);
    for id in ["first", "second", "direct"] {
        let output = fixture.run(&["plugin", "uninstall", id]).await;
        assert!(output.status.success(), "{output:?}");
        assert!(fixture.0.join(format!("home/plugins/{id}.py")).exists());
        let shown = fixture.run(&["plugin", "show", id]).await;
        let value: Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(value[id]["installed"], false);
        assert_eq!(value[id]["enabled"], false);
        assert_eq!(value[id]["loaded"], false);
        if id == "first" {
            let shown = fixture.run(&["plugin", "show", "second"]).await;
            let value: Value = serde_json::from_slice(&shown.stdout).unwrap();
            assert_eq!(value["second"]["loaded"], true);
            assert_eq!(value["second"]["entry_id"], "second");
            assert_eq!(
                value["second"]["chain_order"],
                serde_json::json!(["second"])
            );
        }
    }
    assert_eq!(std::fs::read(server_path).unwrap(), server_before);
}

#[tokio::test]
async fn invalid_installed_plugin_fails_foreground_and_background_service_start() {
    let fixture = Fixture::new();
    let source = fixture.0.join("broken");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("plugin.config"), "{ invalid plugin declaration").unwrap();
    let installed = fixture
        .run(&["plugin", "install", source.to_str().unwrap()])
        .await;
    assert!(installed.status.success(), "{installed:?}");
    // Old state is irrelevant, even if it contains invalid data.
    std::fs::create_dir_all(fixture.0.join("home/plugin-state")).unwrap();
    std::fs::write(
        fixture.0.join("home/plugin-state/active.json"),
        "invalid old state",
    )
    .unwrap();
    for foreground in [true, false] {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        drop(listener);
        let mut arguments = vec!["server", "start", "--port", &port];
        if foreground {
            arguments.push("--foreground");
        }
        let output = fixture.run(&arguments).await;
        assert!(!output.status.success(), "{output:?}");
        if foreground {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("inspect plugin broken"),
                "{output:?}"
            );
        }
        assert!(!fixture.0.join("home/server.config").exists());
    }
}
