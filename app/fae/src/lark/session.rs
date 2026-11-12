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
fn thread_help() -> &'static str {
    "话题外发送普通消息开始新会话；话题内发送消息继续当前会话。\n群聊话题内外均需 @智能体。\n/status, /session  查看当前话题状态\n/todo <message>  规划并执行\n/session clean, /clean  清理当前话题历史和 todos\n/steer <message>  补充当前话题执行\n执行中所有输入自动作为 /steer"
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
        eprintln!("Lark input {id} failed: {safe}");
        let _ = self.store.update(id, |r| {
            if r.session_mapping_version == 2
                && r.thread_state == super::store::ThreadState::AcceptanceUnknown
            {
                r.last_error = Some(safe.clone());
                return;
            }
            r.discard_unsent_responses();
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
    fn typing(self: &Arc<Self>, id: &str) {
        let record = self.store.get(id);
        if record.inbound_thread_id.is_empty()
            || record.reaction_id.is_some()
            || record.state == State::Delivered
        {
            return;
        }
        let gateway = self.clone();
        let receipt = id.to_owned();
        self.spawn(async move {
            if let Ok(reaction) = gateway.sdk.typing(&record.message_id).await {
                let saved = gateway.store.update(&receipt, |r| {
                    r.reaction_id = Some(reaction.clone());
                    // A late API response must re-enter durable cleanup rather than
                    // leave an untracked reaction on an already delivered message.
                    if r.state == State::Delivered {
                        r.state = State::Delivering;
                        r.delivered_at = None;
                    }
                });
                if saved.is_err() {
                    let _ = gateway.sdk.untype(&record.message_id, &reaction).await;
                }
            }
        });
    }
    fn supplement(
        self: &Arc<Self>,
        id: &str,
        round: &str,
        live: &LiveSession,
        recovering: bool,
    ) -> anyhow::Result<()> {
        let owner = live
            .owners
            .get(round)
            .map(|owner| owner.receipt.clone())
            .or_else(|| self.store.round_receipt(&self.store.get(id), round))
            .ok_or_else(|| anyhow::anyhow!("补充已接受，但找不到原执行记录"))?;
        self.store.update(id, |r| {
            r.round_id = Some(round.into());
            r.completion_receipt = Some(owner);
        })?;
        self.notice(id, "已补充到当前执行中".into())?;
        if !recovering {
            self.typing(id);
        }
        Ok(())
    }
    async fn process(self: &Arc<Self>, id: &str) -> anyhow::Result<()> {
        let _admission = self.store.admission(id).await;
        let mut record = self.store.get(id);
        if record.outcome.is_some() {
            return Ok(());
        }
        anyhow::ensure!(!*self.stopping.borrow(), "网关已停止");
        // Apply the current policy to both live input and recovered receipts,
        // before creating a thread, opening a session, or executing commands.
        if !self.config.allows_user(&record.binding.user_id) {
            return self.notice(id, "权限不足".into());
        }
        if let Some(error) = &record.rejection {
            anyhow::bail!("{error}");
        }
        let thread_mode = record.session_mapping_version == 2;
        if thread_mode {
            let command = record.text.trim().to_owned();
            if record.inbound_thread_id.is_empty() && record.thread_create_attempt.is_none() {
                if matches!(
                    command.as_str(),
                    "/help" | "/session -h" | "/session --help"
                ) {
                    return self.notice(id, thread_help().into());
                }
                if command.starts_with('/') && !command.starts_with("/todo ") {
                    return self.notice(id, "请在对应话题中发送命令；在话题外发送普通消息会开始新会话。使用 /help 查看帮助。".into());
                }
            }
            // Only admission is ordered; each predecessor returns once input has
            // been accepted, rather than waiting for its agent round to finish.
            for predecessor in self.store.predecessors(&record) {
                if predecessor.message_id == record.root_id && !record.binding.thread_id.is_empty()
                {
                    let _root_admission = self.store.admission(&predecessor.id).await;
                    super::thread::confirm_root_event(self, &predecessor.id, &record)?;
                }
                if let Err(error) = Box::pin(self.process(&predecessor.id)).await {
                    self.fail(&predecessor.id, &format!("{error:#}"));
                }
                if self.store.get(&predecessor.id).outcome.is_none()
                    && self.store.get(&predecessor.id).state == State::Received
                {
                    self.store.update(id, |r| {
                        r.last_error = Some("话题首条消息仍待确认；本次暂不提交".into())
                    })?;
                    return Ok(());
                }
            }
            record = super::thread::resolve(self, id).await?;
            if command == "/session new" {
                return self.notice(id, "请在话题外发送消息以开始新会话".into());
            }
            if command.starts_with("/session id=") {
                return self.notice(id, "请在对应话题中发送消息以继续会话".into());
            }
        } else {
            anyhow::ensure!(
                record.state != State::Received,
                "旧聊天记录尚未提交；升级后不会自动执行，请发送新的普通消息"
            );
        }
        let key = if thread_mode {
            identity::worker_key(&record.binding, &self.config.agent_id)
        } else {
            identity::session_id(&record.binding)
        };
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
        if live.owners.values().any(|owner| owner.receipt == id) {
            return Ok(());
        }
        anyhow::ensure!(
            identity::owns_session(&record.binding, &record.session_id),
            "Invalid saved Lark session"
        );
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
            let client = Client::open(
                &self.home,
                &server,
                OpenSession {
                    target: SessionTarget::Agent {
                        source: fae_agent::SingleAgentSource::AgentId(self.config.agent_id.clone()),
                    },
                    session_id: Some(record.session_id.clone()),
                    workspace: Some(self.config.workspace.clone()),
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
        anyhow::ensure!(
            selected == record.session_id,
            "会话不匹配；本次不会自动重跑"
        );
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
                "/help" | "/session -h" | "/session --help" => {
                    return self.notice(id, thread_help().into());
                }
                "/status" | "/session" => {
                    return self.notice(
                        id,
                        format!(
                            "session: {selected}\nmodel: {}\nstate: {}",
                            client.info.model, status.state
                        ),
                    );
                }
                "/clean" | "/session clean" => {
                    // Save command intent before the destructive operation; never replay it after a crash.
                    self.store.update(id, |r| {
                        r.state = State::Submitting;
                        r.session_id = selected.clone();
                    })?;
                    request(client, ClientMessage::Clean).await?;
                    live.client = None;
                    return self.notice(
                        id,
                        format!("已清理当前 session 的历史和 todos\nsession: {selected}"),
                    );
                }
                value
                    if value.starts_with('/')
                        && !value.starts_with("/todo ")
                        && !value.starts_with("/steer ") =>
                {
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
            let round = round_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("补充已接受，但未返回原执行 ID"))?;
            return self.supplement(id, round, &live, recovering);
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
        // Thread creation already shows Typing; react only to inputs received inside a thread.
        // Feedback never delays admission or steering.
        if !recovering {
            self.typing(id);
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
    use serde_json::json;

    fn feedback_gateway(base: String) -> Arc<Gateway> {
        let home = std::env::temp_dir().join(format!("fae-feedback-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        let config: Config = serde_json::from_value(json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant",
            "agent_id":"fae","workspace":home
        }))
        .unwrap();
        let (_, stopping) = watch::channel(false);
        Arc::new(Gateway {
            home: home.clone(),
            config: config.clone(),
            store: Arc::new(Store::open(&home, 7).unwrap()),
            sdk: Sdk::for_test(config, base),
            stopping,
            slots: Arc::new(Semaphore::new(1)),
            sessions: Mutex::new(HashMap::new()),
            tasks: std::sync::Mutex::new(tokio::task::JoinSet::new()),
        })
    }

    fn feedback_record(message: &str) -> Record {
        let binding: SessionBinding = serde_json::from_value(json!({
            "session_mapping_version":2,"thread_id":"omt_feedback","channel":"lark",
            "app_id":"app","tenant_key":"tenant","user_id":"ou_user",
            "chat_type":"p2p","chat_id":"chat"
        }))
        .unwrap();
        let mut record = Record::new(
            identity::receipt_id("app", "tenant", message),
            "omt_feedback".into(),
            binding,
            message.into(),
            "input".into(),
            None,
        );
        record.service_instance = Some("service".into());
        record.stream_id = Some("stream".into());
        record
    }

    #[test]
    fn failure_discards_partial_answer_and_preserves_existing_delivery_metadata() {
        let gateway = feedback_gateway("http://127.0.0.1:1".into());
        let mut record = feedback_record("om_failed");
        let mut partial = super::super::card::Card::new("partial".into());
        partial.text = "未完成正文".into();
        record.responses.push(partial.clone());
        let mut delivered = partial.clone();
        delivered.reply_id = Some("om_existing".into());
        delivered.closed = true;
        record.responses.push(delivered);
        let id = record.id.clone();
        gateway.store.insert(record).unwrap();
        gateway.fail(&id, "service disconnected");
        let saved = gateway.store.get(&id);
        assert_eq!(saved.outcome.as_deref(), Some("failed"));
        assert!(saved.notice.unwrap().contains("service disconnected"));
        assert_eq!(saved.responses.len(), 1);
        assert_eq!(saved.responses[0].reply_id.as_deref(), Some("om_existing"));
        let mut uncertain = partial;
        uncertain.reply_attempt = Some(1);
        gateway
            .store
            .update(&id, |r| r.responses = vec![uncertain])
            .unwrap();
        gateway.fail(&id, "again");
        assert_eq!(gateway.store.get(&id).responses.len(), 1);
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test]
    async fn whitelist_denial_replies_without_thread_or_agent_admission() {
        let (base, requests, task) = super::super::sdk::tests::mock(|path, body| {
            assert!(path.ends_with("/reply"));
            assert_eq!(body["msg_type"], "text");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(body["content"].as_str().unwrap())
                    .unwrap()["text"],
                "权限不足"
            );
            json!({"code":0,"data":{"message_id":"om_denied_reply",
                "chat_id":"chat","thread_id":"omt_feedback","root_id":"om_root"}})
        })
        .await;
        let mut gateway = feedback_gateway(base);
        Arc::get_mut(&mut gateway).unwrap().config.user_id_whitelist =
            Some(vec!["ou_other".into()]);
        // These receipts have no saved rejection: recovery must enforce the
        // current whitelist before accepting input under an older policy.
        for (index, text, in_thread) in [
            (0, "hello", false),
            (1, "/help", false),
            (2, "/clean", true),
            (3, "/steer update", true),
        ] {
            let mut record = feedback_record(&format!("om_denied_{index}"));
            record.text = text.into();
            if !in_thread {
                record.binding.thread_id.clear();
                record.session_id.clear();
                record.inbound_thread_id.clear();
                record.thread_state = super::super::store::ThreadState::Unresolved;
            }
            let id = record.id.clone();
            gateway.store.insert(record).unwrap();
            gateway.recover(id.clone()).await;
            let saved = gateway.store.get(&id);
            assert_eq!(saved.notice.as_deref(), Some("权限不足"));
            assert!(saved.round_id.is_none());
            assert!(saved.thread_create_attempt.is_none());
            assert!(saved.reaction_id.is_none());
            assert!(gateway.sessions.lock().await.is_empty());
            super::super::card::Writer {
                sdk: gateway.sdk.clone(),
                store: gateway.store.clone(),
            }
            .flush(&id)
            .await
            .unwrap();
            assert!(gateway.store.get(&id).state == State::Delivered);
        }
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _)| path.ends_with("/reply"))
                .count(),
            4
        );
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test]
    async fn late_typing_response_reenters_durable_cleanup() {
        let receipt = Arc::new(std::sync::Mutex::new(None::<(Arc<Store>, String)>));
        let callback = receipt.clone();
        let (base, _, task) = super::super::sdk::tests::mock(move |path, _| {
            if path.ends_with("/reactions") {
                let saved = callback.lock().unwrap();
                let (store, id) = saved.as_ref().unwrap();
                store
                    .update(id, |r| {
                        r.state = State::Delivered;
                        r.delivered_at = Some(1);
                    })
                    .unwrap();
                json!({"code":0,"data":{"reaction_id":"late"}})
            } else {
                assert!(path.ends_with("/reactions/late"));
                json!({"code":0})
            }
        })
        .await;
        let gateway = feedback_gateway(base);
        let mut record = feedback_record("om_late");
        record.outcome = Some("succeeded".into());
        record.state = State::Delivering;
        let id = record.id.clone();
        gateway.store.insert(record).unwrap();
        *receipt.lock().unwrap() = Some((gateway.store.clone(), id.clone()));
        gateway.typing(&id);
        gateway.shutdown().await;
        let saved = gateway.store.get(&id);
        assert!(saved.state == State::Delivering);
        assert_eq!(saved.reaction_id.as_deref(), Some("late"));
        let writer = super::super::card::Writer {
            sdk: gateway.sdk.clone(),
            store: gateway.store.clone(),
        };
        writer.flush(&id).await.unwrap();
        assert!(gateway.store.get(&id).state == State::Delivered);
        assert!(gateway.store.get(&id).reaction_id.is_none());
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test]
    async fn supplement_reaction_waits_for_final_delivery_and_survives_restart() {
        let mut deletion_failed = false;
        let (base, requests, task) = super::super::sdk::tests::mock(move |path, body| {
            if path.ends_with("/om_follow/reactions") {
                assert_eq!(body["reaction_type"]["emoji_type"], "Typing");
                json!({"code":0,"data":{"reaction_id":"reaction"}})
            } else if path.ends_with("/reactions/reaction") {
                if !deletion_failed {
                    deletion_failed = true;
                    json!({"code":99991400,"msg":"temporary failure"})
                } else {
                    json!({"code":0})
                }
            } else if path.ends_with("/reply") {
                assert_eq!(body["reply_in_thread"], true);
                json!({"code":0,"data":{"message_id":"om_reply","thread_id":"omt_feedback","chat_id":"chat","root_id":"om_parent"}})
            } else {
                panic!("Unexpected feedback request: {path}");
            }
        }).await;
        let gateway = feedback_gateway(base);
        let mut parent = feedback_record("om_parent");
        parent.round_id = Some("round".into());
        parent.state = State::Running;
        let parent_id = parent.id.clone();
        gateway.store.insert(parent).unwrap();
        let follow = feedback_record("om_follow");
        let id = follow.id.clone();
        gateway.store.insert(follow).unwrap();
        gateway
            .supplement(&id, "round", &LiveSession::default(), false)
            .unwrap();
        gateway.shutdown().await;
        assert_eq!(
            gateway.store.get(&id).reaction_id.as_deref(),
            Some("reaction")
        );
        let writer = super::super::card::Writer {
            sdk: gateway.sdk.clone(),
            store: gateway.store.clone(),
        };
        writer.flush(&id).await.unwrap();
        let saved = gateway.store.get(&id);
        assert!(saved.notice_sent);
        assert!(saved.state == State::Delivering);
        assert_eq!(
            saved.completion_receipt.as_deref(),
            Some(parent_id.as_str())
        );
        assert_eq!(saved.reaction_id.as_deref(), Some("reaction"));
        let restarted = super::super::card::Writer {
            sdk: gateway.sdk.clone(),
            store: Arc::new(Store::open(&gateway.home, 7).unwrap()),
        };
        restarted.flush(&id).await.unwrap();
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.ends_with("/reactions/reaction"))
                .count(),
            0
        );
        restarted
            .store
            .update(&parent_id, |r| {
                r.state = State::Delivering;
                r.outcome = Some("succeeded".into());
                r.notice = Some("最终答复".into());
            })
            .unwrap();
        restarted.flush(&parent_id).await.unwrap();
        assert!(restarted.flush(&id).await.is_err());
        assert_eq!(
            restarted.store.get(&id).reaction_id.as_deref(),
            Some("reaction")
        );
        restarted.flush(&id).await.unwrap();
        assert!(restarted.store.get(&id).state == State::Delivered);
        assert!(restarted.store.get(&id).reaction_id.is_none());
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.ends_with("/om_follow/reply"))
                .count(),
            1
        );
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn idle_thread_commands_preserve_binding_and_clean_connections() {
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
            session_mapping_version: 2,
            thread_id: "omt_commands".into(),
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
        assert!(
            submit("new", "/session new".into())
                .await
                .contains("话题外")
        );
        assert!(
            submit("selected", "/session".into())
                .await
                .contains(&original)
        );
        assert!(submit("clean", "/clean".into()).await.contains(&original));
        // Clean terminates the actor; the following status must open a fresh connection.
        assert!(
            submit("after-clean", "/status".into())
                .await
                .contains(&original)
        );
        assert!(
            submit("switch", format!("/session id={original}"))
                .await
                .contains("对应话题")
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
}
