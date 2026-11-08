mod card;
pub mod config;
pub mod identity;
mod inbound;
pub mod lifecycle;
mod output;
mod sdk;
mod session;
mod store;

use crate::server::lifecycle::{FileLock, atomic_write};
use larksuite_oapi_sdk_rs::{EventDispatcher, LarkError};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Semaphore, mpsc, watch};

pub async fn check_online(config: config::Config) -> anyhow::Result<()> {
    sdk::Sdk::new(config)?.bot_id().await?;
    Ok(())
}

pub async fn run(home: PathBuf) -> anyhow::Result<()> {
    lifecycle::prepare(&home)?;
    let _lock = FileLock::try_acquire(&home.join("lark/gateway.lock"))?
        .ok_or_else(|| anyhow::anyhow!("Lark gateway already running"))?;
    let config = config::Config::load(&home)?;
    config.check_agent(&home).await?;
    crate::app::daemon::ensure(&home).await?;
    let sdk = sdk::Sdk::new(config.clone())?;
    let bot = sdk.bot_id().await?;
    let store = Arc::new(store::Store::open(
        &home,
        config.storage.completed_record_retention_days,
    )?);
    let (incoming, receiver) = mpsc::channel::<String>(1024);
    let (shutdown, stopping) = watch::channel(false);
    let gateway = Arc::new(session::Gateway {
        home: home.clone(),
        config: config.clone(),
        store: store.clone(),
        sdk: sdk.clone(),
        stopping: stopping.clone(),
        slots: Arc::new(Semaphore::new(config.execution.max_concurrent_rounds)),
    });
    let mut router = tokio::spawn(gateway.clone().route(receiver));
    for record in store.pending() {
        if record.outcome.is_none() {
            incoming.send(record.id).await?;
        }
    }
    let outbox_store = store.clone();
    let outbox_sdk = sdk.clone();
    let mut outbox_stopping = stopping.clone();
    let mut outbox = tokio::spawn(async move {
        let writer = card::Writer {
            sdk: outbox_sdk.clone(),
            store: outbox_store.clone(),
        };
        let mut tick =
            tokio::time::interval(Duration::from_millis(config.output.flush_interval_ms));
        let mut retries = std::collections::HashMap::<String, (u32, tokio::time::Instant)>::new();
        loop {
            tokio::select! { _ = outbox_stopping.changed() => break, _ = tick.tick() => {} }
            for record in outbox_store.pending() {
                if retries
                    .get(&record.id)
                    .is_some_and(|(_, at)| *at > tokio::time::Instant::now())
                {
                    continue;
                }
                match writer.flush(&record.id).await {
                    Ok(()) => {
                        retries.remove(&record.id);
                    }
                    Err(error) => {
                        let entry = retries
                            .entry(record.id.clone())
                            .or_insert((0, tokio::time::Instant::now()));
                        entry.0 += 1;
                        entry.1 = tokio::time::Instant::now()
                            + Duration::from_secs(1u64 << entry.0.min(6));
                        let safe = outbox_sdk.sanitize(&error.to_string());
                        let _ = outbox_store.update(&record.id, |r| {
                            r.last_error = Some(safe);
                            if entry.0 == 3 {
                                for card in &mut r.responses {
                                    if !card.closed
                                        && (card.reply_id.is_some() || card.reply_attempt.is_none())
                                    {
                                        card.degraded = true;
                                        if card.reply_id.is_some() {
                                            card.reply_uuid = wd_tools::uuid::v4();
                                            card.reply_attempt = None;
                                        }
                                    }
                                }
                            }
                        });
                    }
                }
            }
        }
    });
    let event_store = store.clone();
    let event_config = gateway.config.clone();
    let event_shutdown = stopping.clone();
    let dispatcher = EventDispatcher::new("", "").on_p2_im_message_receive_v1(move |event| {
        let incoming = incoming.clone();
        let store = event_store.clone();
        let config = event_config.clone();
        let bot = bot.clone();
        let stopping = event_shutdown.clone();
        async move {
            if *stopping.borrow() {
                return Err(LarkError::Event("Gateway stopping".into()));
            }
            let record = inbound::parse(&config, &bot, event)
                .map_err(|e| LarkError::Event(e.to_string()))?;
            if let Some(record) = record {
                let permit = incoming
                    .clone()
                    .try_reserve_owned()
                    .map_err(|_| LarkError::Event("Gateway ingress queue full".into()))?;
                let id = record.id.clone();
                // A slow filesystem must not hold the SDK callback past its ACK window.
                // The reserved queue slot stays with the persistence task even on timeout.
                let persistence = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    if store.insert(record)? {
                        permit.send(id);
                    }
                    Ok(())
                });
                tokio::time::timeout(Duration::from_secs(2), persistence)
                    .await
                    .map_err(|_| LarkError::Event("Receipt persistence timed out".into()))?
                    .map_err(|e| LarkError::Event(e.to_string()))?
                    .map_err(|e| LarkError::Event(e.to_string()))?;
            }
            Ok(())
        }
    });
    let (ready, mut readiness) = watch::channel(false);
    let ready_disconnect = ready.clone();
    let ws = sdk
        .client
        .ws_client(dispatcher)
        .on_ready(move || {
            let _ = ready.send(true);
        })
        .on_disconnected(move || {
            let _ = ready_disconnect.send(false);
        });
    let ws_task = tokio::spawn(async move { ws.start().await });
    let mut runtime = lifecycle::Runtime::new();
    let (control, mut stop_rx) = lifecycle::control_server(&home, &runtime, store.clone()).await?;
    runtime.port = control;
    atomic_write(
        &home.join("lark/runtime.json"),
        &serde_json::to_vec(&runtime)?,
        true,
    )?;
    let signal = crate::server::lifecycle::stop_signal()?;
    tokio::pin!(signal);
    let mut ws_task = ws_task;
    let result = loop {
        tokio::select! {
            _ = &mut signal => break Ok(()),
            _ = stop_rx.recv() => break Ok(()),
            changed = readiness.changed() => {
                if changed.is_err() { break Err(anyhow::anyhow!("Feishu connection closed")); }
                runtime.ready = *readiness.borrow();
                atomic_write(&home.join("lark/runtime.json"), &serde_json::to_vec(&runtime)?, true)?;
            }
            result = &mut ws_task => break result.map_err(anyhow::Error::from).and_then(|r| r.map_err(Into::into)),
        }
    };
    let _ = shutdown.send(true);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    ws_task.abort();
    stop_task(&mut router, deadline).await;
    stop_task(&mut outbox, deadline).await;
    // The outbox has exited before this writer takes ownership. One deadline
    // bounds the whole shutdown; unflushed records remain durable for recovery.
    let writer = card::Writer {
        sdk: sdk.clone(),
        store: store.clone(),
    };
    for record in store.pending() {
        if record.outcome.is_none() {
            gateway.fail(&record.id, "网关停止，本次执行已取消");
        }
    }
    for record in store.pending() {
        let until = deadline.min(tokio::time::Instant::now() + Duration::from_secs(2));
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let _ = tokio::time::timeout_at(until, writer.flush(&record.id)).await;
    }
    let _ = std::fs::remove_file(home.join("lark/runtime.json"));
    result
}

async fn stop_task(task: &mut tokio::task::JoinHandle<()>, deadline: tokio::time::Instant) {
    let until = deadline.min(tokio::time::Instant::now() + Duration::from_secs(10));
    if tokio::time::timeout_at(until, &mut *task).await.is_err() {
        task.abort();
        // Dropping a JoinHandle detaches it. Await cancellation before allowing
        // any replacement writer to read or mutate the same persisted cards.
        let _ = task.await;
    }
}
