use fae::{
    app::client::{self, Client},
    protocol::*,
};
use fae_agent::{SessionInput, SessionInputData, SingleAgentSource};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU16, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

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
            ControlMessage::Initialize(InitArgsDto {
                agent_id: id.into(),
                model: "mock".into(),
                force: false,
            }),
        )
        .await
        .unwrap();
        std::fs::write(
            self.home.join(format!("agents/{id}_prompt.txt")),
            "You are a test agent.",
        )
        .unwrap();
    }
    fn agent(&self, session: &str) -> OpenSession {
        OpenSession {
            binding: None,
            target: SessionTarget::Agent {
                source: SingleAgentSource::AgentId("fae".into()),
            },
            session_id: Some(session.into()),
            workspace: self.home.clone(),
            resume: None,
        }
    }
    fn workflow(&self, id: &str, session: &str) -> OpenSession {
        OpenSession {
            binding: None,
            target: SessionTarget::Workflow {
                workflow_id: id.into(),
                input: json!({}),
            },
            session_id: Some(session.into()),
            workspace: self.home.clone(),
            resume: None,
        }
    }
    fn write_workflow(&self, id: &str, code: &str) {
        std::fs::create_dir_all(self.home.join("workflows")).unwrap();
        std::fs::write(
            self.home.join(format!("workflows/{id}.json")),
            serde_json::to_vec(&json!({
                "id":id,"desc":"Controlled process test","nodes":{
                    "start":{"type":"start","next":"python"},
                    "python":{"type":"execute","action":{"type":"python","code":code},"next":"end"},
                    "end":{"type":"end","output":42}
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }
    async fn stop(&mut self) {
        assert!(fae::app::daemon::stop(&self.home).await.unwrap());
        assert!(self.child.wait().unwrap().success());
        assert!(!self.home.join("server.config").exists());
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lark_binds_raw_user_and_deduplicates_external_input() {
    let (base, mut requests, model_task) = model().await;
    let mut service = Service::start(Some(&base)).await;
    service.init("fae").await;
    let path = service.home.join("agents/fae_config.json");
    let mut agent: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    agent["tools"] = json!([]);
    agent["skills"] = json!([]);
    std::fs::write(&path, serde_json::to_vec(&agent).unwrap()).unwrap();
    std::fs::write(
        service.home.join("lark.config"),
        serde_json::to_vec(&json!({
            "version":1, "app_id":"cli_test", "app_secret":"test-secret",
            "tenant_key":"tenant", "agent_id":"fae", "workspace":service.home
        }))
        .unwrap(),
    )
    .unwrap();
    let binding = SessionBinding {
        channel: "lark".into(),
        user_id: "alice".into(),
        app_id: "cli_test".into(),
        tenant_key: "tenant".into(),
        chat_type: "group".into(),
        chat_id: "group".into(),
    };
    let mut open = service.agent(&fae::lark::identity::session_id(&binding));
    open.binding = Some(binding.clone());
    let mut alice = Client::open(&service.home, &service.config, open.clone())
        .await
        .unwrap();
    assert_eq!(alice.info.user_id, "alice");
    let request_id = fae::lark::identity::receipt_id("cli_test", "tenant", "message");
    let body = ClientMessage::ExternalInput {
        external_request_id: request_id.clone(),
        input: SessionInput::NewChat("hello".into()),
    };
    let round = match alice.request(body.clone()).await.unwrap() {
        ServerMessage::Ack { round_id: Some(id) } => id,
        other => panic!("{other:?}"),
    };
    match alice.request(body).await.unwrap() {
        ServerMessage::Acceptance {
            round_id: Some(id),
            uncertain: false,
            ..
        } => assert_eq!(id, round),
        other => panic!("{other:?}"),
    }
    assert!(
        alice
            .request(ClientMessage::ExternalInput {
                external_request_id: request_id,
                input: SessionInput::NewChat("different".into())
            })
            .await
            .is_err()
    );
    assert!(matches!(
        finish(&mut alice).await.1,
        RoundOutcome::Succeeded { .. }
    ));
    assert_eq!(requests.recv().await.unwrap(), "hello");
    assert!(
        requests.try_recv().is_err(),
        "duplicate request executed the model"
    );
    let history = service
        .home
        .join(format!("session/fae/alice/{}.jsonl", alice.info.session_id));
    assert!(
        std::fs::read_to_string(history)
            .unwrap()
            .contains("mock answer")
    );
    // A different user's canonical session cannot resume another user's stream.
    let mut bob = binding;
    bob.user_id = "bob".into();
    open.binding = Some(bob.clone());
    open.session_id = Some(fae::lark::identity::session_id(&bob));
    open.resume = Some(Resume {
        stream_id: alice.stream.clone(),
        after_seq: 0,
    });
    assert!(
        Client::open(&service.home, &service.config, open.clone())
            .await
            .is_err()
    );
    open.resume = None;
    let mut bob = Client::open(&service.home, &service.config, open)
        .await
        .unwrap();
    assert_eq!(bob.info.user_id, "bob");
    // The restricted credential cannot perform cleanup.
    assert!(bob.request(ClientMessage::Clean).await.is_err());
    bob.request(ClientMessage::Close {
        cancel_active: false,
    })
    .await
    .unwrap();
    alice
        .request(ClientMessage::Close {
            cancel_active: false,
        })
        .await
        .unwrap();
    service.stop().await;
    model_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lark_resume_preserves_replay_and_frozen_config_for_all_outcomes() {
    let (base, mut requests, model_task) = model().await;
    let mut service = Service::start(Some(&base)).await;
    service.init("fae").await;
    let config_path = service.home.join("agents/fae_config.json");
    let prompt_path = service.home.join("agents/fae_prompt.txt");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["tools"] = json!([]);
    config["skills"] = json!([]);
    let config_bytes = serde_json::to_vec(&config).unwrap();
    let prompt_bytes = std::fs::read(&prompt_path).unwrap();
    std::fs::write(
        service.home.join("lark.config"),
        serde_json::to_vec(&json!({
            "version":1, "app_id":"cli_test", "app_secret":"test-secret",
            "tenant_key":"tenant", "agent_id":"fae", "workspace":service.home
        }))
        .unwrap(),
    )
    .unwrap();
    for expected in ["succeeded", "failed", "cancelled"] {
        std::fs::write(&config_path, &config_bytes).unwrap();
        std::fs::write(&prompt_path, &prompt_bytes).unwrap();
        let binding = SessionBinding {
            channel: "lark".into(),
            app_id: "cli_test".into(),
            tenant_key: "tenant".into(),
            user_id: "alice".into(),
            chat_type: "group".into(),
            chat_id: expected.into(),
        };
        let mut open = service.agent(&fae::lark::identity::session_id(&binding));
        open.binding = Some(binding);
        let history = service
            .home
            .join(format!("session/fae/alice/{}.jsonl", open.session_id.as_ref().unwrap()));
        let mut client = Client::open(&service.home, &service.config, open.clone())
            .await
            .unwrap();
        let receipt = fae::lark::identity::receipt_id("cli_test", "tenant", expected);
        let input = match expected {
            "cancelled" => "stream then wait",
            "failed" => "stream before save",
            _ => "hello",
        };
        let round = match client
            .request(ClientMessage::ExternalInput {
                external_request_id: receipt.clone(),
                input: SessionInput::NewChat(input.into()),
            })
            .await
            .unwrap()
        {
            ServerMessage::Ack { round_id: Some(round) } => round,
            other => panic!("{other:?}"),
        };
        assert_eq!(requests.recv().await.unwrap(), input);
        if expected != "succeeded" {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let ServerMessage::Event { output, .. } = client.next().await.unwrap() {
                        if output.input_type == "model_output" {
                            assert_eq!(output.output["content"], "mock answer");
                            break;
                        }
                    }
                }
            })
            .await
            .unwrap();
            if expected == "cancelled" {
                client.request(ClientMessage::Cancel { round_id: round.clone() }).await.unwrap();
            } else {
                // Saving history fails after model text has already been emitted.
                std::fs::create_dir_all(&history).unwrap();
            }
        }
        let outcome = finish(&mut client).await.1;
        assert!(matches!(
            (expected, &outcome),
            ("succeeded", RoundOutcome::Succeeded { .. })
                | ("failed", RoundOutcome::Failed { .. })
                | ("cancelled", RoundOutcome::Cancelled { .. })
        ));
        let stream = client.stream.clone();
        client.request(ClientMessage::Close { cancel_active: false }).await.unwrap();
        drop(client);
        std::fs::remove_file(&config_path).unwrap();
        std::fs::remove_file(&prompt_path).unwrap();
        open.resume = Some(Resume { stream_id: stream, after_seq: 0 });
        let mut resumed = Client::open(&service.home, &service.config, open)
            .await
            .unwrap();
        assert!(resumed.info.resumed);
        // The response query buffers earlier replay. A terminal receipt must
        // not make the consumer drop that buffered model text.
        assert!(matches!(
            resumed.request(ClientMessage::Acceptance { external_request_id: receipt }).await.unwrap(),
            ServerMessage::Acceptance { outcome: Some(_), .. }
        ));
        let text = tokio::time::timeout(Duration::from_secs(10), async {
            let mut text = String::new();
            let mut last_seq = 0;
            loop {
                let message = resumed.next().await.unwrap();
                if let Some(seq) = message.seq() {
                    assert!(seq > last_seq);
                    last_seq = seq;
                }
                match message {
                    ServerMessage::Event { round_id, output, .. } => {
                        assert_eq!(round_id, round);
                        if output.input_type == "model_output" {
                            text.push_str(output.output["content"].as_str().unwrap());
                        }
                    }
                    ServerMessage::Finished { round_id, outcome: replayed, .. } => {
                        assert_eq!(round_id, round);
                        assert_eq!(serde_json::to_value(replayed).unwrap(), serde_json::to_value(&outcome).unwrap());
                        break text;
                    }
                    ServerMessage::Error(error) => panic!("{error}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(text, "mock answer");
        assert!(requests.try_recv().is_err(), "resume reran the model");
        resumed.request(ClientMessage::Close { cancel_active: false }).await.unwrap();
    }
    service.stop().await;
    model_task.abort();
}

async fn input(client: &mut Client, text: &str) -> String {
    match client
        .request(ClientMessage::Input {
            input: SessionInput::NewChat(SessionInputData::text(text)),
            mode: None,
        })
        .await
        .unwrap()
    {
        ServerMessage::Ack { round_id: Some(id) } => id,
        other => panic!("expected ack: {other:?}"),
    }
}
async fn finish(client: &mut Client) -> (String, RoundOutcome) {
    let mut observed = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let message = client.next().await.unwrap();
            observed.push(format!("{message:?}"));
            match message {
                ServerMessage::Finished {
                    round_id, outcome, ..
                } => return (round_id, outcome),
                ServerMessage::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
    })
    .await;
    match result {
        Ok(result) => result,
        Err(error) => {
            let status = tokio::time::timeout(
                Duration::from_secs(2),
                client.request(ClientMessage::Status),
            )
            .await;
            panic!("finish timed out: {error}; status={status:?}; events={observed:?}");
        }
    }
}

// Tiny SSE model server exercises the production ModelRuntime and streaming
// path. No credentials or process-global environment changes are needed.
async fn model() -> (String, mpsc::Receiver<String>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel(32);
    let task = tokio::spawn(async move {
        let mut requests = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                Some(_) = requests.join_next(), if !requests.is_empty() => {},
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    let tx = tx.clone();
                    requests.spawn(async move {
                        let mut bytes = Vec::new();
                        let body = loop {
                            let mut buffer = [0; 4096];
                            let read = socket.read(&mut buffer).await.unwrap();
                            if read == 0 { return; }
                            bytes.extend_from_slice(&buffer[..read]);
                            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                                let header = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                                let length: usize = header.lines().find_map(|line| line.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap();
                                if bytes.len() >= end + 4 + length {
                                    break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + length]).unwrap();
                                }
                            }
                        };
                        let text = body["messages"].as_array().unwrap().iter().rev()
                            .find(|m| m["role"] == "user").unwrap()["content"].as_str().unwrap().to_string();
                        tx.send(text.clone()).await.unwrap();
                        if matches!(text.as_str(), "stream then wait" | "stream before save") {
                            let chunk = json!({"id":"mock","object":"chat.completion.chunk","created":1,"model":"mock",
                                "choices":[{"index":0,"delta":{"role":"assistant","content":"mock answer"},"finish_reason":null}]});
                            let data = format!("data: {chunk}\n\n");
                            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{data}\r\n", data.len());
                            let _ = socket.write_all(response.as_bytes()).await;
                            if text == "stream then wait" {
                                tokio::time::sleep(Duration::from_secs(30)).await;
                            } else {
                                tokio::time::sleep(Duration::from_millis(400)).await;
                                let end = "data: [DONE]\n\n";
                                let _ = socket.write_all(format!("{:x}\r\n{end}\r\n0\r\n\r\n", end.len()).as_bytes()).await;
                            }
                            return;
                        }
                        if text.contains("slow") { tokio::time::sleep(Duration::from_secs(30)).await; }
                        let tool_result = body["messages"].as_array().unwrap().last().is_some_and(|m| m["role"] == "tool");
                        let (delta, reason) = if text.starts_with("timer:create:") && !tool_result {
                            use chrono::{Datelike, Timelike};
                            let at = chrono::Utc::now() + chrono::Duration::seconds(4);
                            let schedule = if text.ends_with("repeat") {
                                json!({"kind":"repeat","cron":"0/2 * * * * *","max_fires":3})
                            } else {
                                json!({"kind":"once","cron":format!("{} {} {} {} {} * {}", at.second(), at.minute(), at.hour(), at.day(), at.month(), at.year())})
                            };
                            let args = json!({"operation":"create","idempotency_key":text,"name":"Service timer","input":"background timer input","schedule":schedule});
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"timer-1","type":"function","function":{"name":"timer","arguments":args.to_string()}}]}), "tool_calls")
                        } else if text == "delegate" {
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"agent","arguments":"{\"agent_id\":\"worker\",\"input\":\"slow child\"}"}}]}), "tool_calls")
                        } else { (json!({"role":"assistant","content":"mock answer"}), "stop") };
                        let chunk = |delta: Value, finish: Value| json!({"id":"mock","object":"chat.completion.chunk","created":1,"model":"mock",
                            "choices":[{"index":0,"delta":delta,"finish_reason":finish}]}).to_string();
                        let data = format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n", chunk(delta, Value::Null), chunk(json!({}), json!(reason)));
                        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len());
                        let _ = socket.write_all(response.as_bytes()).await;
                    });
                }
            }
        }
    });
    (base, rx, task)
}

async fn timer_state(service: &Service, user: &str, accepted: u64, terminal: usize) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(bytes) =
                std::fs::read(service.home.join(format!("timer/fae/{user}/state.json")))
            {
                let state: Value = serde_json::from_slice(&bytes).unwrap();
                let done = state["occurrences"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|o| o["status"] == "succeeded")
                    .count();
                if state["timers"][0]["accepted_count"] == accepted && done >= terminal {
                    return state;
                }
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap_or_else(|e| {
        panic!(
            "timer timeout ({accepted}/{terminal}): {e}; state={:?}; log={:?}",
            std::fs::read_to_string(service.home.join(format!("timer/fae/{user}/state.json"))),
            std::fs::read_to_string(service.home.join("startup.log"))
        )
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timer_runs_without_client_after_clean_and_config_user_change() {
    let (base, mut requests, model_task) = model().await;
    let mut service = Service::start(Some(&base)).await;
    service.init("fae").await;
    let config_path = service.home.join("agents/fae_config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["agent"]["user_id"] = json!("alice");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut client = Client::open(
        &service.home,
        &service.config,
        service.agent("timer-origin"),
    )
    .await
    .unwrap();
    input(&mut client, "timer:create:once").await;
    assert!(matches!(
        finish(&mut client).await.1,
        RoundOutcome::Succeeded { .. }
    ));
    let state = timer_state(&service, "alice", 0, 0).await;
    assert_eq!(state["timers"][0]["created_session_id"], "timer-origin");
    assert!(matches!(
        client.request(ClientMessage::Clean).await.unwrap(),
        ServerMessage::Cleaned
    ));
    drop(client);
    config["agent"]["user_id"] = json!("bob");
    config["agent"]["session_id"] = json!("changed-default");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let state = timer_state(&service, "alice", 1, 1).await;
    let o = &state["occurrences"][0];
    assert_eq!(o["owner"]["user_id"], "alice");
    assert_eq!(o["status"], "succeeded");
    let session = o["session_id"].as_str().unwrap();
    assert_ne!(session, "timer-origin");
    assert_ne!(session, "changed-default");
    let history = std::fs::read_to_string(
        service
            .home
            .join(format!("session/fae/alice/{session}.jsonl")),
    )
    .unwrap();
    assert!(history.contains("background timer input"));
    assert!(history.contains("mock answer"));
    assert!(
        !service
            .home
            .join(format!("session/fae/bob/{session}.jsonl"))
            .exists()
    );
    assert!(
        !service
            .home
            .join("session/fae/alice/timer-origin.jsonl")
            .exists()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if requests.recv().await.unwrap() == "background timer input" {
                    break;
                }
            }
        })
        .await
        .is_ok()
    );
    service.stop().await;
    model_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeat_timer_recovers_after_service_restart_with_distinct_sessions() {
    let (base, _requests, model_task) = model().await;
    let mut service = Service::start(Some(&base)).await;
    service.init("fae").await;
    let mut client = Client::open(
        &service.home,
        &service.config,
        service.agent("repeat-origin"),
    )
    .await
    .unwrap();
    input(&mut client, "timer:create:repeat").await;
    assert!(matches!(
        finish(&mut client).await.1,
        RoundOutcome::Succeeded { .. }
    ));
    client
        .request(ClientMessage::Close {
            cancel_active: false,
        })
        .await
        .unwrap();
    drop(client);
    timer_state(&service, "master", 1, 1).await;
    service.stop().await;
    service.child = Command::new(env!("CARGO_BIN_EXE_fae"))
        .arg("--fae-home")
        .arg(&service.home)
        .args(["server", "start", "--foreground", "--port"])
        .arg(service.config.port.to_string())
        .env("OPENAI_BASE_URL", &base)
        .env("OPENAI_API_KEY", "test-key")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(config) = fae::server::lifecycle::read_config(&service.home) {
                if client::hello(&service.home, &config).await.is_ok() {
                    service.config = config;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let state = timer_state(&service, "master", 3, 3).await;
    let sessions: std::collections::HashSet<_> = state["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(sessions.len(), 3);
    assert_eq!(state["timers"][0]["state"], "completed");
    for session in sessions {
        assert_ne!(session, "repeat-origin");
        assert!(
            service
                .home
                .join(format!("session/fae/master/{session}.jsonl"))
                .exists()
        );
    }
    service.stop().await;
    model_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_support_multiple_rounds_cancel_current_and_delegated_contexts() {
    let (base, mut requests, model_task) = model().await;
    let mut service = Service::start(Some(&base)).await;
    service.init("fae").await;
    service.init("worker").await;
    let path = service.home.join("agents/fae_config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["sub_agents"] = json!(["worker"]);
    std::fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    let open = service.agent("multi");
    let mut client = Client::open(&service.home, &service.config, open.clone())
        .await
        .unwrap();
    assert!(
        Client::open(&service.home, &service.config, open.clone())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("session_in_use")
    );
    let first = input(&mut client, "hello").await;
    assert_eq!(requests.recv().await.unwrap(), "hello");
    let (round, outcome) = finish(&mut client).await;
    assert_eq!(round, first);
    assert!(matches!(outcome, RoundOutcome::Succeeded { .. }));
    let second = input(&mut client, "slow second").await;
    assert_eq!(requests.recv().await.unwrap(), "slow second");
    assert!(
        client
            .request(ClientMessage::Input {
                input: SessionInput::NewChat(SessionInputData::text("busy")),
                mode: None
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("session_busy")
    );
    let mut other = Client::open(&service.home, &service.config, service.agent("other"))
        .await
        .unwrap();
    assert!(
        other
            .request(ClientMessage::Clean)
            .await
            .unwrap_err()
            .to_string()
            .contains("cleanup_busy")
    );
    client
        .request(ClientMessage::Cancel {
            round_id: second.clone(),
        })
        .await
        .unwrap();
    let (round, outcome) = finish(&mut client).await;
    assert_eq!(round, second);
    assert!(matches!(outcome, RoundOutcome::Cancelled { .. }));
    let delegated = input(&mut client, "delegate").await;
    assert_eq!(requests.recv().await.unwrap(), "delegate");
    assert_eq!(requests.recv().await.unwrap(), "slow child");
    client
        .request(ClientMessage::Cancel {
            round_id: delegated,
        })
        .await
        .unwrap();
    assert!(matches!(
        finish(&mut client).await.1,
        RoundOutcome::Cancelled { .. }
    ));
    let ServerMessage::History { messages } = client
        .request(ClientMessage::History {
            limit: 100,
            offset: 0,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(!messages.is_empty());
    assert!(matches!(
        client.request(ClientMessage::Clean).await.unwrap(),
        ServerMessage::Cleaned
    ));
    assert!(!service.home.join("session/fae/master/multi.jsonl").exists());
    service.stop().await;
    model_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workflow_disconnect_replay_dedup_authentication_and_shutdown() {
    let mut service = Service::start(None).await;
    service.write_workflow("delayed", "import time\ntime.sleep(0.4)\nresult = 42");
    let open = service.workflow("delayed", "replay");
    let mut client = Client::open(&service.home, &service.config, open.clone())
        .await
        .unwrap();
    let stream = client.stream.clone();
    // Close only the client lease; the workflow keeps running.
    client
        .request(ClientMessage::Close {
            cancel_active: false,
        })
        .await
        .unwrap();
    drop(client);
    let mut resume = open;
    resume.resume = Some(Resume {
        stream_id: stream,
        after_seq: 0,
    });
    let mut client = Client::open(&service.home, &service.config, resume)
        .await
        .unwrap();
    assert!(client.info.resumed);
    assert!(
        matches!(finish(&mut client).await.1, RoundOutcome::Succeeded { result } if result == json!(42))
    );
    client
        .request(ClientMessage::Close {
            cancel_active: false,
        })
        .await
        .unwrap();

    let mut expired = service.workflow("delayed", "replay");
    expired.resume = Some(Resume {
        stream_id: "expired-stream".into(),
        after_seq: 0,
    });
    assert!(
        Client::open(&service.home, &service.config, expired)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("resume_expired")
    );

    service.write_workflow("slow", "import time\ntime.sleep(30)\nresult = 42");
    let mut client = Client::open(
        &service.home,
        &service.config,
        service.workflow("slow", "cancel"),
    )
    .await
    .unwrap();
    let round = loop {
        if let ServerMessage::Ack {
            round_id: Some(round),
        } = client.next().await.unwrap()
        {
            break round;
        }
    };
    client
        .request(ClientMessage::Cancel { round_id: round })
        .await
        .unwrap();
    assert!(matches!(
        finish(&mut client).await.1,
        RoundOutcome::Cancelled { .. }
    ));

    let request = service
        .config
        .endpoint("/v1/control")
        .into_client_request()
        .unwrap();
    assert!(tokio_tungstenite::connect_async(request).await.is_err());
    let mut request = service
        .config
        .endpoint("/v1/control")
        .into_client_request()
        .unwrap();
    let token = std::fs::read_to_string(service.home.join("server.token")).unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    request
        .headers_mut()
        .insert("origin", "http://localhost".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(request).await.is_err());

    // Raw connection allows an intentionally repeated request ID.
    service.init("fae").await;
    let mut socket = client::connect(&service.home, &service.config, "/v1/session")
        .await
        .unwrap();
    async fn exchange(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        id: &str,
        body: ClientMessage,
    ) -> ServerMessage {
        socket
            .send(Message::Text(
                serde_json::to_string(&ClientEnvelope {
                    version: VERSION,
                    request_id: id.into(),
                    body,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => {
                    return serde_json::from_str::<ServerEnvelope>(&text).unwrap().body;
                }
                Message::Ping(data) => socket.send(Message::Pong(data)).await.unwrap(),
                _ => {}
            }
        }
    }
    assert!(matches!(
        exchange(
            &mut socket,
            "open",
            ClientMessage::Open(service.agent("dedup"))
        )
        .await,
        ServerMessage::Opened(_)
    ));
    let first = exchange(&mut socket, "same", ClientMessage::Status).await;
    let second = exchange(&mut socket, "same", ClientMessage::Status).await;
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(second).unwrap()
    );
    assert!(
        matches!(exchange(&mut socket, "same", ClientMessage::Clean).await, ServerMessage::Error(error) if error.code == "request_id_conflict")
    );
    service.stop().await;
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
    for name in ["server.token", "server.config"] {
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

#[test]
#[cfg(unix)]
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

    std::fs::create_dir_all(service.home.join("log")).unwrap();
    std::fs::write(
        service.home.join("log/server.log"),
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
        std::fs::read_to_string(service.home.join("log/server.log"))
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
    assert!(
        fae::server::lifecycle::FileLock::try_acquire(&service.home.join("server.lock"))
            .unwrap()
            .is_none()
    );
    assert!(fae::app::daemon::stop(&service.home).await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_workflow_opens_and_session_capacity_remain_isolated() {
    let mut service = Service::start(None).await;
    service.write_workflow("quick", "result = 42");
    let mut open = service.workflow("quick", "unused");
    open.session_id = None;
    let mut first = Client::open(&service.home, &service.config, open.clone())
        .await
        .unwrap();
    let mut second = Client::open(&service.home, &service.config, open)
        .await
        .unwrap();
    assert_ne!(first.stream, second.stream);
    assert_ne!(first.info.session_id, second.info.session_id);
    assert!(matches!(
        finish(&mut first).await.1,
        RoundOutcome::Succeeded { .. }
    ));
    assert!(matches!(
        finish(&mut second).await.1,
        RoundOutcome::Succeeded { .. }
    ));
    service.init("fae").await;
    let mut clients = Vec::new();
    for id in 0..30 {
        clients.push(
            Client::open(
                &service.home,
                &service.config,
                service.agent(&format!("idle-{id}")),
            )
            .await
            .unwrap(),
        );
    }
    assert!(
        Client::open(&service.home, &service.config, service.agent("overflow"))
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("resource_exhausted")
    );
    let mut conflicting = service.agent("idle-0");
    std::fs::create_dir(service.home.join("other-workspace")).unwrap();
    conflicting.workspace = service.home.join("other-workspace");
    assert!(
        Client::open(&service.home, &service.config, conflicting)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("session_binding_conflict")
    );
    service.stop().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn activity_limit_does_not_block_status_or_shutdown_cancellation() {
    let mut service = Service::start(None).await;
    service.write_workflow("slow", "import time\ntime.sleep(30)\nresult = 42");
    let mut clients = Vec::new();
    for index in 0..8 {
        let mut client = Client::open(
            &service.home,
            &service.config,
            service.workflow("slow", &format!("active-{index}")),
        )
        .await
        .unwrap();
        loop {
            if matches!(
                client.next().await.unwrap(),
                ServerMessage::Ack { round_id: Some(_) }
            ) {
                break;
            }
        }
        clients.push(client);
    }
    let mut rejected = Client::open(
        &service.home,
        &service.config,
        service.workflow("slow", "overflow"),
    )
    .await
    .unwrap();
    assert!(
        matches!(rejected.next().await.unwrap(), ServerMessage::Error(error) if error.code == "resource_exhausted")
    );
    assert!(
        matches!(clients[0].request(ClientMessage::Status).await.unwrap(), ServerMessage::Status(status) if status.state == "running")
    );
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_client_answers_heartbeats_without_foreground_reads() {
    let mut service = Service::start(None).await;
    service.init("fae").await;
    let mut client = Client::open(
        &service.home,
        &service.config,
        service.agent("idle-heartbeat"),
    )
    .await
    .unwrap();
    // Simulate the user editing for longer than the server's 45s timeout.
    tokio::time::sleep(Duration::from_secs(50)).await;
    assert!(
        matches!(client.request(ClientMessage::Status).await.unwrap(), ServerMessage::Status(status) if status.state == "idle")
    );
    service.stop().await;
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
    assert!(!service.home.join("server.token").exists());
    assert!(
        fae::server::lifecycle::FileLock::try_acquire(&service.home.join("server.lock"))
            .unwrap()
            .is_some()
    );
}
