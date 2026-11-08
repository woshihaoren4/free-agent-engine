use super::{
    config::Config,
    output,
    sdk::Sdk,
    store::{State, Store},
};
use crate::{app::client::Client, protocol::*};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Semaphore, mpsc, watch};

pub struct Gateway {
    pub home: PathBuf,
    pub config: Config,
    pub store: Arc<Store>,
    pub sdk: Sdk,
    pub stopping: watch::Receiver<bool>,
    pub slots: Arc<Semaphore>,
}
impl Gateway {
    pub fn fail(&self, id: &str, error: &str) {
        let safe = self.sdk.sanitize(error);
        let _ = self.store.update(id, |r| {
            r.state = State::Delivering;
            r.outcome = Some("failed".into());
            r.notice = Some(format!("ERROR：{safe}\n追踪：{}", &r.id[..12]));
        });
    }
    async fn process(self: &Arc<Self>, id: &str) -> anyhow::Result<()> {
        let record = self.store.get(id);
        if record.outcome.is_some() {
            return Ok(());
        }
        if *self.stopping.borrow() {
            anyhow::bail!("网关停止，本次执行已取消");
        }
        if let Some(rejection) = &record.rejection {
            self.fail(id, rejection);
            return Ok(());
        }
        let mut stopping = self.stopping.clone();
        let permit = tokio::select! {
            permit = self.slots.clone().acquire_owned() => permit?,
            _ = stopping.changed() => { anyhow::bail!("网关停止，本次执行已取消"); }
        };
        let recovering = record.state != State::Received;
        let server = loop {
            match crate::app::daemon::discover(&self.home).await {
                Ok(Some(config)) => break config,
                _ if recovering => anyhow::bail!("服务不可用，本次执行已中断；不会自动重跑"),
                _ => {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                        _ = stopping.changed() => anyhow::bail!("网关已停止"),
                    }
                }
            }
        };
        if recovering {
            anyhow::ensure!(
                record.service_instance.as_deref() == Some(&server.instance_id),
                "服务重启，本次执行已中断；不会自动重跑"
            );
        }
        let mut client = Client::open(
            &self.home,
            &server,
            OpenSession {
                target: SessionTarget::Agent {
                    source: fae_agent::SingleAgentSource::AgentId(self.config.agent_id.clone()),
                },
                session_id: Some(record.session_id.clone()),
                workspace: self.config.workspace.clone(),
                binding: Some(record.binding.clone()),
                resume: if recovering {
                    Some(Resume {
                        stream_id: record.stream_id.clone().ok_or_else(|| {
                            anyhow::anyhow!("Acceptance unknown; stream identity missing")
                        })?,
                        after_seq: record.seq,
                    })
                } else {
                    None
                },
            },
        )
        .await?;
        if !recovering {
            self.store.update(id, |r| {
                r.stream_id = Some(client.stream.clone());
                r.service_instance = Some(server.instance_id.clone());
                r.state = State::Submitting;
            })?;
            let accepted = client
                .request(ClientMessage::ExternalInput {
                    external_request_id: record.id.clone(),
                    input: fae_agent::SessionInput::NewChat(record.text.clone().into()),
                })
                .await;
            let accepted = match accepted {
                Ok(response) => response,
                Err(_) => {
                    client
                        .request(ClientMessage::Acceptance {
                            external_request_id: record.id.clone(),
                        })
                        .await?
                }
            };
            let round = match accepted {
                ServerMessage::Ack {
                    round_id: Some(round),
                }
                | ServerMessage::Acceptance {
                    round_id: Some(round),
                    uncertain: false,
                    ..
                } => round,
                _ => {
                    self.store
                        .update(id, |r| r.state = State::AcceptanceUnknown)?;
                    anyhow::bail!("输入接受状态不确定，请重新发起；本次不会自动重跑");
                }
            };
            self.store.update(id, |r| {
                r.round_id = Some(round);
                r.state = State::Running;
            })?;
        } else if record.round_id.is_none() {
            let accepted = client
                .request(ClientMessage::Acceptance {
                    external_request_id: record.id.clone(),
                })
                .await?;
            if let ServerMessage::Acceptance {
                round_id: Some(round),
                uncertain: false,
                ..
            } = accepted
            {
                self.store.update(id, |r| {
                    r.round_id = Some(round);
                    r.state = State::Running;
                })?;
            } else {
                anyhow::bail!("输入接受状态不确定；本次不会自动重跑");
            }
        }
        let round_id = self.store.get(id).round_id.unwrap();
        // Acceptance reports execution state, not replay completion. Drain the
        // stream through this round's Finished before committing its outcome.
        // Only an explicit replay gap permits recovery from the terminal result.
        let mut incomplete = false;
        loop {
            let message = tokio::select! {
                _ = stopping.changed() => {
                    let _ = client.request(ClientMessage::Cancel { round_id: round_id.clone() }).await;
                    self.store.update(id, |r| {
                        r.outcome = Some("cancelled".into()); r.state = State::Delivering;
                        r.notice = Some("本次执行已取消".into());
                    })?;
                    break;
                }
                message = client.next() => message?,
            };
            match &message {
                ServerMessage::Event {
                    round_id: event_round,
                    ..
                }
                | ServerMessage::Finished {
                    round_id: event_round,
                    ..
                } if event_round != &round_id => continue,
                ServerMessage::Error(error) if error.code == "resume_gap" => {
                    incomplete = true;
                    // Do not concatenate deltas after an unobserved section.
                    self.store.update(id, |r| {
                        r.last_error = Some("Service replay gap; output incomplete".into());
                        for card in &mut r.responses {
                            card.ended = true;
                        }
                    })?;
                    if let ServerMessage::Acceptance {
                        outcome: Some(outcome),
                        ..
                    } = client
                        .request(ClientMessage::Acceptance {
                            external_request_id: record.id.clone(),
                        })
                        .await?
                    {
                        self.store.update(id, |r| {
                            output::recover_terminal(r, &round_id, outcome);
                            if let Some(notice) = &r.notice {
                                r.notice = Some(self.sdk.sanitize(notice));
                            }
                        })?;
                        break;
                    }
                    continue;
                }
                ServerMessage::Error(error) => anyhow::bail!("{error}"),
                _ => {}
            }
            if incomplete && matches!(message, ServerMessage::Event { .. }) {
                continue;
            }
            self.store.update(id, |r| {
                if incomplete {
                    if let ServerMessage::Finished { outcome, .. } = &message {
                        output::recover_terminal(r, &round_id, outcome.clone());
                    }
                } else {
                    output::ingest(r, &message);
                }
                if let Some(notice) = &r.notice {
                    r.notice = Some(self.sdk.sanitize(notice));
                }
            })?;
            if matches!(message, ServerMessage::Finished { .. }) {
                break;
            }
        }
        let _ = client
            .request(ClientMessage::Close {
                cancel_active: false,
            })
            .await;
        drop(permit);
        Ok(())
    }
    pub async fn route(self: Arc<Self>, mut incoming: mpsc::Receiver<String>) {
        let mut workers: HashMap<String, mpsc::Sender<String>> = HashMap::new();
        let mut tasks = tokio::task::JoinSet::new();
        let mut stopping = self.stopping.clone();
        loop {
            let id = tokio::select! {
                _ = stopping.changed() => break,
                Some(_) = tasks.join_next(), if !tasks.is_empty() => { continue; },
                id = incoming.recv() => match id { Some(id) => id, None => break },
            };
            let record = self.store.get(&id);
            let sdk = self.sdk.clone();
            let store = self.store.clone();
            let receipt = id.clone();
            // Feedback is independent of both session FIFO and Agent concurrency.
            tasks.spawn(async move {
                if store.get(&receipt).reaction_id.is_none()
                    && store.get(&receipt).outcome.is_none()
                {
                    match sdk.typing(&record.message_id).await {
                        Ok(reaction) => {
                            let mut finished = false;
                            let saved = store.update(&receipt, |r| {
                                finished = r.outcome.is_some();
                                if !finished {
                                    r.reaction_id = Some(reaction.clone());
                                }
                            });
                            if finished || saved.is_err() {
                                let _ = sdk.untype(&record.message_id, &reaction).await;
                            }
                        }
                        Err(e) => {
                            let _ = store.update(&receipt, |r| {
                                r.last_error = Some(sdk.sanitize(&e.to_string()))
                            });
                        }
                    }
                }
            });
            let key = self.store.get(&id).session_id;
            if workers.get(&key).is_none_or(|tx| tx.is_closed()) {
                let (tx, mut rx) =
                    mpsc::channel::<String>(self.config.execution.session_queue_capacity);
                workers.insert(key.clone(), tx);
                let gateway = self.clone();
                tasks.spawn(async move {
                    while let Ok(Some(id)) = tokio::time::timeout(
                        Duration::from_secs(gateway.config.execution.session_idle_secs),
                        rx.recv(),
                    )
                    .await
                    {
                        if let Err(error) = gateway.process(&id).await {
                            gateway.fail(&id, &format!("{error:#}"));
                        }
                        if *gateway.stopping.borrow() {
                            break;
                        }
                    }
                });
            }
            if workers[&key].try_send(id.clone()).is_err() {
                self.fail(&id, "当前会话待处理消息过多，请稍后再试");
            }
            workers.retain(|_, tx| !tx.is_closed());
        }
        drop(workers);
        while tasks.join_next().await.is_some() {}
    }
}
