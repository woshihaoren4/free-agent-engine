use super::{
    config::Config,
    identity, output,
    sdk::Sdk,
    store::{State, Store},
};
use crate::{app::client::Client, protocol::*};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, watch};

struct Owner {
    receipt: String,
    _permit: OwnedSemaphorePermit,
    incomplete: bool,
}
#[derive(Default)]
pub struct LiveSession {
    selected: String,
    client: Option<Client>,
    owners: HashMap<String, Owner>,
    monitoring: bool,
}
pub struct Gateway {
    pub home: PathBuf,
    pub config: Config,
    pub store: Arc<Store>,
    pub sdk: Sdk,
    pub stopping: watch::Receiver<bool>,
    pub slots: Arc<Semaphore>,
    pub sessions: Mutex<HashMap<String, Arc<Mutex<LiveSession>>>>,
    pub tasks: std::sync::Mutex<tokio::task::JoinSet<()>>,
}
impl Gateway {
    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.tasks.lock().unwrap();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(future);
    }
    pub fn dispatch(self: &Arc<Self>, id: String) {
        if *self.stopping.borrow() {
            self.fail(&id, "网关已停止");
            return;
        }
        let gateway = self.clone();
        self.spawn(async move {
            if let Err(error) = gateway.process(&id).await {
                gateway.fail(&id, &format!("{error:#}"));
            }
        });
    }
    pub async fn recover(self: &Arc<Self>, id: String) {
        if let Err(error) = self.process(&id).await {
            self.fail(&id, &format!("{error:#}"));
        }
    }
    pub async fn shutdown(&self) {
        let mut tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
        while tasks.join_next().await.is_some() {}
    }
    pub fn fail(&self, id: &str, error: &str) {
        let safe = self.sdk.sanitize(error);
        let _ = self.store.update(id, |r| {
            r.state = State::Delivering;
            r.outcome = Some("failed".into());
            r.notice = Some(format!("ERROR：{safe}\n追踪：{}", &r.id[..12]));
        });
    }
    fn notice(&self, id: &str, text: String) -> anyhow::Result<()> {
        self.store.update(id, |r| {
            r.state = State::Delivering;
            r.outcome = Some("succeeded".into());
            r.notice = Some(text);
        })?;
        Ok(())
    }
    fn selection_path(&self, binding: &SessionBinding) -> PathBuf {
        self.home
            .join("lark/sessions")
            .join(format!("{}.json", identity::session_id(binding)))
    }
    fn select(
        &self,
        live: &mut LiveSession,
        binding: &SessionBinding,
        id: String,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            identity::owns_session(binding, &id),
            "只能切换到当前用户和聊天所属的 session"
        );
        let path = self.selection_path(binding);
        std::fs::create_dir_all(path.parent().unwrap())?;
        crate::server::lifecycle::atomic_write(&path, &serde_json::to_vec(&id)?, true)?;
        live.selected = id;
        Ok(())
    }
    async fn process(self: &Arc<Self>, id: &str) -> anyhow::Result<()> {
        let record = self.store.get(id);
        if record.outcome.is_some() {
            return Ok(());
        }
        anyhow::ensure!(!*self.stopping.borrow(), "网关已停止");
        if let Some(error) = &record.rejection {
            anyhow::bail!("{error}");
        }
        let key = identity::session_id(&record.binding);
        let shared = self
            .sessions
            .lock()
            .await
            .entry(key.clone())
            .or_default()
            .clone();
        // Serialize admission only. No lock or queue waits for an agent round to finish.
        let mut live = shared.lock().await;
        anyhow::ensure!(!*self.stopping.borrow(), "网关已停止");
        if live.selected.is_empty() {
            live.selected = match std::fs::read(self.selection_path(&record.binding)) {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => key,
                Err(e) => return Err(e.into()),
            };
            anyhow::ensure!(
                identity::owns_session(&record.binding, &live.selected),
                "Invalid saved Lark session"
            );
        }
        let recovering = record.state != State::Received;
        let server = crate::app::daemon::discover(&self.home)
            .await?
            .ok_or_else(|| anyhow::anyhow!("服务不可用；本次不会排队或自动重跑"))?;
        if recovering {
            anyhow::ensure!(
                record.service_instance.as_deref() == Some(&server.instance_id),
                "服务重启，本次不会自动重跑"
            );
        }
        if live.client.is_none() {
            let selected = if recovering {
                record.session_id.clone()
            } else {
                live.selected.clone()
            };
            let client = Client::open(
                &self.home,
                &server,
                OpenSession {
                    target: SessionTarget::Agent {
                        source: fae_agent::SingleAgentSource::AgentId(self.config.agent_id.clone()),
                    },
                    session_id: Some(selected),
                    workspace: self.config.workspace.clone(),
                    binding: Some(record.binding.clone()),
                    resume: if recovering {
                        Some(Resume {
                            stream_id: record.stream_id.clone().ok_or_else(|| {
                                anyhow::anyhow!("接受状态不确定；本次不会自动重跑")
                            })?,
                            after_seq: record.seq,
                        })
                    } else {
                        None
                    },
                },
            )
            .await?;
            live.client = Some(client);
        }
        if !live.monitoring {
            live.monitoring = true;
            let gateway = self.clone();
            let shared = shared.clone();
            self.spawn(async move {
                gateway.monitor(shared).await;
            });
        }
        let client = live.client.as_mut().unwrap();
        let selected = client.info.session_id.clone();
        if recovering {
            anyhow::ensure!(
                selected == record.session_id,
                "恢复会话不匹配；本次不会自动重跑"
            );
        }
        let ServerMessage::Status(status) = request(client, ClientMessage::Status).await? else {
            anyhow::bail!("Invalid session status");
        };
        let busy = status.round_id.is_some();
        // Status may overtake the local monitor after Finished was buffered by
        // request(). Commit that output and release its slot before idle commands.
        if !busy {
            while !live.owners.is_empty() {
                let message = tokio::time::timeout(
                    Duration::from_secs(2),
                    live.client.as_mut().unwrap().next_connected(),
                )
                .await
                .map_err(|_| anyhow::anyhow!("正在保存上一轮结果，请稍后重试"))??;
                if let ServerMessage::Error(error) = &message {
                    if error.code == "resume_gap" {
                        self.recover_gap(&mut live).await;
                        continue;
                    }
                    anyhow::bail!("{error}");
                }
                self.ingest(&mut live, &message)?;
            }
        }
        let client = live.client.as_mut().unwrap();
        let command = record.text.trim();
        if !recovering && !busy {
            match command {
                "/help" | "/session -h" | "/session --help" => return self.notice(id,
                    "/status  查看状态\n/todo <message>  规划并执行\n/session new  新建会话\n/session id=<id>  切换会话\n/session clean, /clean  清理历史和 todos\n/steer <message>  补充执行中的指令\n执行中所有输入自动作为 /steer".into()),
                "/status" | "/session" => return self.notice(id, format!("session: {selected}\nmodel: {}\nstate: {}", client.info.model, status.state)),
                "/clean" | "/session clean" => {
                    // Save command intent before the destructive operation; never replay it after a crash.
                    self.store.update(id, |r| { r.state = State::Submitting; r.session_id = selected.clone(); })?;
                    request(client, ClientMessage::Clean).await?;
                    live.client = None;
                    return self.notice(id, format!("已清理当前 session 的历史和 todos\nsession: {selected}"));
                }
                "/session new" => {
                    self.store.update(id, |r| r.state = State::Submitting)?;
                    request(client, ClientMessage::Close { cancel_active: false }).await?;
                    live.client = None;
                    self.select(&mut live, &record.binding, format!("{}--{}", identity::session_id(&record.binding), wd_tools::uuid::v4()))?;
                    return self.notice(id, format!("session: {}", live.selected));
                }
                value if value.starts_with("/session id=") => {
                    let target = value.trim_start_matches("/session id=");
                    anyhow::ensure!(identity::owns_session(&record.binding, target), "只能切换到当前用户和聊天所属的 session");
                    self.store.update(id, |r| r.state = State::Submitting)?;
                    request(client, ClientMessage::Close { cancel_active: false }).await?;
                    live.client = None;
                    self.select(&mut live, &record.binding, target.into())?;
                    return self.notice(id, format!("session: {}", live.selected));
                }
                value if value.starts_with('/') && !value.starts_with("/todo ") && !value.starts_with("/steer ") => {
                    anyhow::bail!("未知命令或缺少参数，请使用 /help");
                }
                _ => {}
            }
        }
        let permit = if !busy && !recovering {
            Some(
                self.slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| anyhow::anyhow!("当前执行数已达上限，请稍后重试；本次不会排队"))?,
            )
        } else {
            None
        };
        let client = live.client.as_mut().unwrap();
        let accepted = if recovering {
            request(
                client,
                ClientMessage::Acceptance {
                    external_request_id: id.into(),
                },
            )
            .await?
        } else {
            let (input, mode) = if let Some(text) = command.strip_prefix("/steer ") {
                anyhow::ensure!(!text.trim().is_empty(), "/steer 需要补充内容");
                (fae_agent::SessionInput::Supplement(text.into()), None)
            } else if busy {
                (
                    fae_agent::SessionInput::Supplement(record.text.clone().into()),
                    None,
                )
            } else if let Some(text) = command.strip_prefix("/todo ") {
                anyhow::ensure!(!text.trim().is_empty(), "/todo 需要任务内容");
                (
                    fae_agent::SessionInput::NewChat(text.into()),
                    Some("todo".into()),
                )
            } else {
                (
                    fae_agent::SessionInput::NewChat(record.text.clone().into()),
                    None,
                )
            };
            self.store.update(id, |r| {
                r.session_id = selected.clone();
                r.stream_id = Some(client.stream.clone());
                r.service_instance = Some(server.instance_id.clone());
                r.state = State::Submitting;
            })?;
            match request(
                client,
                ClientMessage::ExternalInput {
                    external_request_id: id.into(),
                    input,
                    mode,
                    auto_steer: true,
                },
            )
            .await
            {
                Ok(response) => response,
                Err(error) => {
                    match request(
                        client,
                        ClientMessage::Acceptance {
                            external_request_id: id.into(),
                        },
                    )
                    .await
                    {
                        Ok(
                            response @ ServerMessage::Acceptance {
                                round_id: Some(_),
                                uncertain: false,
                                ..
                            },
                        ) => response,
                        _ => {
                            self.store
                                .update(id, |r| r.state = State::AcceptanceUnknown)?;
                            return Err(error.context("输入未确认；本次不会自动重跑"));
                        }
                    }
                }
            }
        };
        if let ServerMessage::Acceptance {
            supplemented: true,
            uncertain: false,
            round_id,
            ..
        } = &accepted
        {
            self.store.update(id, |r| r.round_id = round_id.clone())?;
            return self.notice(id, "已补充到当前执行中".into());
        }
        let round = match accepted {
            ServerMessage::Ack {
                round_id: Some(round),
            }
            | ServerMessage::Acceptance {
                round_id: Some(round),
                uncertain: false,
                ..
            } => round,
            _ => anyhow::bail!("输入接受状态不确定；本次不会自动重跑"),
        };
        // A recovered steering receipt needs no execution slot. Reserve a slot
        // only after acceptance identifies a round whose output we must monitor.
        let permit = match permit {
            Some(permit) => permit,
            None if recovering => self
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| anyhow::anyhow!("执行数已达上限，无法恢复本次输出"))?,
            None => anyhow::bail!("会话刚刚结束；请重新发送，本次不会排队"),
        };
        self.store.update(id, |r| {
            r.round_id = Some(round.clone());
            r.state = State::Running;
        })?;
        live.owners.insert(
            round,
            Owner {
                receipt: id.into(),
                _permit: permit,
                incomplete: false,
            },
        );
        // Feedback never delays admission or steering.
        if !recovering && record.reaction_id.is_none() {
            let gateway = self.clone();
            let receipt = id.to_owned();
            self.spawn(async move {
                if let Ok(reaction) = gateway.sdk.typing(&record.message_id).await {
                    let mut finished = false;
                    let saved = gateway.store.update(&receipt, |r| {
                        finished = r.outcome.is_some();
                        if !finished {
                            r.reaction_id = Some(reaction.clone());
                        }
                    });
                    if finished || saved.is_err() {
                        let _ = gateway.sdk.untype(&record.message_id, &reaction).await;
                    }
                }
            });
        }
        Ok(())
    }
    async fn monitor(self: Arc<Self>, shared: Arc<Mutex<LiveSession>>) {
        let mut idle = tokio::time::Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        loop {
            tick.tick().await;
            let mut live = shared.lock().await;
            if *self.stopping.borrow() {
                let rounds: Vec<_> = live.owners.keys().cloned().collect();
                for round in rounds {
                    if let Some(client) = &mut live.client {
                        let _ = request(client, ClientMessage::Cancel { round_id: round }).await;
                    }
                }
                for (_, owner) in live.owners.drain() {
                    self.fail(&owner.receipt, "网关停止，本次执行已取消");
                }
                live.client = None;
                live.monitoring = false;
                return;
            }
            if live.owners.is_empty() {
                if idle.elapsed().as_secs() >= self.config.execution.session_idle_secs {
                    if let Some(client) = &mut live.client {
                        let _ = request(
                            client,
                            ClientMessage::Close {
                                cancel_active: false,
                            },
                        )
                        .await;
                    }
                    live.client = None;
                    live.monitoring = false;
                    return;
                }
            } else {
                idle = tokio::time::Instant::now();
            }
            let Some(client) = &mut live.client else {
                continue;
            };
            // Transport::receive is cancellation safe. Bound the read so admission can
            // use this same connection while the model is waiting or streaming.
            let message = match tokio::time::timeout(
                Duration::from_millis(50),
                client.next_connected(),
            )
            .await
            {
                Err(_) => continue,
                Ok(Ok(message)) => message,
                Ok(Err(error)) => {
                    if tokio::time::timeout(Duration::from_secs(10), client.reconnect())
                        .await
                        .is_ok_and(|result| result.is_ok())
                    {
                        continue;
                    }
                    for (_, owner) in live.owners.drain() {
                        self.fail(&owner.receipt, &format!("{error:#}"));
                    }
                    live.client = None;
                    live.monitoring = false;
                    return;
                }
            };
            if let ServerMessage::Error(error) = &message {
                if error.code == "resume_gap" {
                    self.recover_gap(&mut live).await;
                    continue;
                }
                for (_, owner) in live.owners.drain() {
                    self.fail(&owner.receipt, &error.to_string());
                }
            }
            if let Err(error) = self.ingest(&mut live, &message) {
                for (_, owner) in live.owners.drain() {
                    self.fail(&owner.receipt, &format!("无法保存执行输出：{error}"));
                }
                live.client = None;
                live.monitoring = false;
                return;
            }
        }
    }
    fn ingest(&self, live: &mut LiveSession, message: &ServerMessage) -> anyhow::Result<()> {
        let round = match message {
            ServerMessage::Event { round_id, .. } | ServerMessage::Finished { round_id, .. } => {
                round_id
            }
            _ => return Ok(()),
        };
        if let Some(owner) = live.owners.get(round) {
            self.store.update(&owner.receipt, |r| {
                if owner.incomplete {
                    if let ServerMessage::Finished { outcome, .. } = message {
                        output::recover_terminal(r, round, outcome.clone());
                    }
                } else {
                    output::ingest(r, message);
                }
                if let Some(text) = &r.notice {
                    r.notice = Some(self.sdk.sanitize(text));
                }
            })?;
            if matches!(message, ServerMessage::Finished { .. }) {
                live.owners.remove(round);
            }
        }
        Ok(())
    }
    async fn recover_gap(&self, live: &mut LiveSession) {
        let receipts: Vec<_> = live
            .owners
            .iter()
            .map(|(round, owner)| (round.clone(), owner.receipt.clone()))
            .collect();
        for (round, receipt) in receipts {
            live.owners.get_mut(&round).unwrap().incomplete = true;
            let _ = self.store.update(&receipt, |r| {
                r.last_error = Some("Service replay gap; output incomplete".into());
                for card in &mut r.responses {
                    card.ended = true;
                }
            });
            if let Ok(ServerMessage::Acceptance {
                outcome: Some(outcome),
                ..
            }) = request(
                live.client.as_mut().unwrap(),
                ClientMessage::Acceptance {
                    external_request_id: receipt.clone(),
                },
            )
            .await
            {
                if self
                    .store
                    .update(&receipt, |r| {
                        output::recover_terminal(r, &round, outcome);
                        if let Some(text) = &r.notice {
                            r.notice = Some(self.sdk.sanitize(text));
                        }
                    })
                    .is_ok()
                {
                    live.owners.remove(&round);
                }
            }
        }
    }
}
async fn request(client: &mut Client, message: ClientMessage) -> anyhow::Result<ServerMessage> {
    tokio::time::timeout(Duration::from_secs(10), client.request(message))
        .await
        .map_err(|_| anyhow::anyhow!("接受状态不确定：服务请求超时"))?
}

#[cfg(test)]
mod tests {
    use super::super::store::Record;
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn idle_gateway_commands_reuse_and_replace_connections() {
        let home = std::env::temp_dir().join(format!("fae-lark-commands-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        crate::server::init::initialize(
            &home,
            &InitArgsDto {
                agent_id: "fae".into(),
                model: "unused".into(),
                force: false,
            },
        )
        .await
        .unwrap();
        std::fs::write(home.join("agents/fae_prompt.txt"), "Test agent.").unwrap();
        Config::init(&home, home.clone(), false).unwrap();
        let mut config: Config =
            serde_json::from_slice(&std::fs::read(home.join("lark.config")).unwrap()).unwrap();
        config.app_id = "test".into();
        config.app_secret = "unused".into();
        config.tenant_key = "tenant".into();
        std::fs::write(
            home.join("lark.config"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let server_home = home.clone();
        let server = tokio::spawn(async move {
            crate::server::run(server_home, "127.0.0.1".parse().unwrap(), port).await
        });
        let service = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(Some(service)) = crate::app::daemon::discover(&home).await {
                    break service;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let (shutdown, stopping) = watch::channel(false);
        let gateway = Arc::new(Gateway {
            home: home.clone(),
            config: config.clone(),
            store: Arc::new(Store::open(&home, 7).unwrap()),
            sdk: Sdk::for_test(config, "http://127.0.0.1:1".into()),
            stopping,
            slots: Arc::new(Semaphore::new(1)),
            sessions: Mutex::new(HashMap::new()),
            tasks: std::sync::Mutex::new(tokio::task::JoinSet::new()),
        });
        let binding = SessionBinding {
            channel: "lark".into(),
            app_id: "test".into(),
            tenant_key: "tenant".into(),
            user_id: "ou_alice".into(),
            chat_type: "p2p".into(),
            chat_id: "chat".into(),
        };
        let original = identity::session_id(&binding);
        let submit = |message: &'static str, text: String| {
            let gateway = gateway.clone();
            let binding = binding.clone();
            async move {
                let id = identity::receipt_id("test", "tenant", message);
                gateway
                    .store
                    .insert(Record::new(
                        id.clone(),
                        identity::session_id(&binding),
                        binding,
                        message.into(),
                        text,
                        None,
                    ))
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), gateway.process(&id))
                    .await
                    .unwrap()
                    .unwrap();
                gateway.store.get(&id).notice.unwrap()
            }
        };
        assert!(submit("help", "/help".into()).await.contains("/steer"));
        assert!(submit("status", "/status".into()).await.contains(&original));
        let next = submit("new", "/session new".into())
            .await
            .trim_start_matches("session: ")
            .to_owned();
        assert_ne!(next, original);
        assert!(identity::owns_session(&binding, &next));
        assert!(submit("selected", "/session".into()).await.contains(&next));
        assert!(submit("clean", "/clean".into()).await.contains(&next));
        // Clean terminates the actor; the following status must open a fresh connection.
        assert!(
            submit("after-clean", "/status".into())
                .await
                .contains(&next)
        );
        assert!(
            submit("switch", format!("/session id={original}"))
                .await
                .contains(&original)
        );
        assert!(
            submit("after-switch", "/status".into())
                .await
                .contains(&original)
        );
        let occupied = gateway.slots.clone().try_acquire_owned().unwrap();
        let blocked = identity::receipt_id("test", "tenant", "blocked");
        gateway
            .store
            .insert(Record::new(
                blocked.clone(),
                original.clone(),
                binding.clone(),
                "blocked".into(),
                "must not queue".into(),
                None,
            ))
            .unwrap();
        gateway.dispatch(blocked.clone());
        tokio::time::timeout(Duration::from_secs(5), async {
            while gateway.store.get(&blocked).outcome.is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            gateway.store.get(&blocked).outcome.as_deref(),
            Some("failed")
        );
        assert!(
            gateway
                .store
                .get(&blocked)
                .notice
                .unwrap()
                .contains("不会排队")
        );
        drop(occupied);
        assert!(
            submit("after-rejection", "/status".into())
                .await
                .contains("idle")
        );
        shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), gateway.shutdown())
            .await
            .unwrap();
        crate::app::client::control(&home, &service, ControlMessage::Shutdown)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(gateway);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[tokio::test]
    async fn session_selection_survives_restart_and_rejects_other_bindings() {
        let home =
            std::env::temp_dir().join(format!("fae-lark-selection-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        Config::init(&home, home.clone(), false).unwrap();
        let config: Config =
            serde_json::from_slice(&std::fs::read(home.join("lark.config")).unwrap()).unwrap();
        let (_, stopping) = watch::channel(false);
        let gateway = Gateway {
            home: home.clone(),
            config: config.clone(),
            store: Arc::new(Store::open(&home, 7).unwrap()),
            sdk: Sdk::for_test(config, "http://127.0.0.1:1".into()),
            stopping,
            slots: Arc::new(Semaphore::new(1)),
            sessions: Mutex::new(HashMap::new()),
            tasks: std::sync::Mutex::new(tokio::task::JoinSet::new()),
        };
        let binding = SessionBinding {
            channel: "lark".into(),
            user_id: "ou_alice".into(),
            app_id: "app".into(),
            tenant_key: "tenant".into(),
            chat_type: "p2p".into(),
            chat_id: "chat".into(),
        };
        let selected = format!(
            "{}--{}",
            identity::session_id(&binding),
            wd_tools::uuid::v4()
        );
        let mut live = LiveSession::default();
        gateway
            .select(&mut live, &binding, selected.clone())
            .unwrap();
        assert_eq!(live.selected, selected);
        let saved: String =
            serde_json::from_slice(&std::fs::read(gateway.selection_path(&binding)).unwrap())
                .unwrap();
        assert_eq!(saved, selected);
        let mut other = binding.clone();
        other.user_id = "ou_bob".into();
        assert!(gateway.select(&mut live, &other, selected.clone()).is_err());
        assert!(!gateway.selection_path(&other).exists());
        assert_eq!(live.selected, selected);
        assert!(
            gateway
                .select(&mut live, &binding, "../escape".into())
                .is_err()
        );
        drop(gateway);
        std::fs::remove_dir_all(home).unwrap();
    }
}
