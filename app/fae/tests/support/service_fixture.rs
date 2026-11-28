use fae::{
    app::client::{self, Client},
    protocol::*,
};
use fae_agent::protocol::API_VERSION;
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
use tokio_tungstenite::tungstenite::Message;

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
                protocol_version: API_VERSION,
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
                        let messages: Vec<Value> = body["input"].as_array().unwrap().iter().map(|item| {
                            if item["type"] == "function_call_output" {
                                json!({"role": "tool", "content": item["output"]})
                            } else {
                                let content = if item["content"].is_array() {
                                    item["content"].as_array().unwrap().iter()
                                        .filter_map(|part| part["text"].as_str())
                                        .collect::<Vec<_>>().join("\n")
                                } else { item["content"].as_str().unwrap_or_default().into() };
                                json!({"role": item["role"], "content": content})
                            }
                        }).collect();
                        let text = messages.iter().rev()
                            .find(|m| m["role"] == "user").unwrap()["content"].as_str().unwrap().to_string();
                        tx.send(text.clone()).await.unwrap();
                        if matches!(text.as_str(), "stream then wait" | "stream before save") {
                            let (data, end) = responses_events(&json!({"content":"mock answer"}));
                            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{data}\r\n", data.len());
                            let _ = socket.write_all(response.as_bytes()).await;
                            if text == "stream then wait" {
                                tokio::time::sleep(Duration::from_secs(30)).await;
                            } else {
                                tokio::time::sleep(Duration::from_millis(400)).await;
                                let _ = socket.write_all(format!("{:x}\r\n{end}\r\n0\r\n\r\n", end.len()).as_bytes()).await;
                            }
                            return;
                        }
                        if text.contains("slow") { tokio::time::sleep(Duration::from_secs(30)).await; }
                        let tool_result = messages.last().is_some_and(|m| m["role"] == "tool");
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
                        } else if text == "workspace:probe" && !tool_result {
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"workspace-1","type":"function","function":{"name":"execute_command","arguments":"{\"command\":\"pwd\"}"}}]}), "tool_calls")
                        } else if text == "workspace:probe" {
                            let prompt = messages.iter()
                                .filter(|m| m["role"] == "system")
                                .map(|m| m["content"].as_str().unwrap_or_default())
                                .collect::<Vec<_>>().join("\n");
                            let tool = messages.last().unwrap()["content"].clone();
                            (json!({"role":"assistant","content":json!({"prompt":prompt,"tool":tool}).to_string()}), "stop")
                        } else if text == "lark:probe" {
                            let prompt = messages.iter()
                                .filter(|m| m["role"] == "system")
                                .map(|m| m["content"].as_str().unwrap_or_default())
                                .collect::<Vec<_>>().join("\n");
                            (json!({"role":"assistant","content":json!({"prompt":prompt,"query":text}).to_string()}), "stop")
                        } else if text == "delegate" {
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"agent","arguments":"{\"agent_id\":\"worker\",\"input\":\"slow child\"}"}}]}), "tool_calls")
                        } else { (json!({"role":"assistant","content":"mock answer"}), "stop") };
                        let _ = reason;
                        let (start, end) = responses_events(&delta);
                        let data = format!("{start}{end}");
                        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len());
                        let _ = socket.write_all(response.as_bytes()).await;
                    });
                }
            }
        }
    });
    (base, rx, task)
}

fn responses_events(delta: &Value) -> (String, String) {
    let (event, output) = if let Some(call) = delta["tool_calls"].as_array().and_then(|calls| calls.first()) {
        let item = json!({"type":"function_call", "id":call["id"], "call_id":call["id"],
            "name":call["function"]["name"], "arguments":call["function"]["arguments"], "status":"completed"});
        (json!({"type":"response.output_item.added", "sequence_number":1,
            "output_index":0, "item":item}), json!([item]))
    } else {
        let text = delta["content"].as_str().unwrap_or_default();
        (json!({"type":"response.output_text.delta", "sequence_number":1,
            "item_id":"msg", "output_index":0, "content_index":0, "delta":text}),
         json!([{"type":"message", "id":"msg", "role":"assistant", "status":"completed",
            "content":[{"type":"output_text", "text":text, "annotations":[]}]}]))
    };
    let complete = json!({"type":"response.completed", "sequence_number":2,
        "response":{"id":"mock", "object":"response", "created_at":1, "model":"mock",
            "status":"completed", "output":output}});
    (format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap()),
     format!("event: response.completed\ndata: {complete}\n\n"))
}
