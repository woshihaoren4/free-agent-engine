use super::registry::Registry;
use fae_agent::{EventType, Runtime, TimerFired, TimerOccurrenceStatus};
use fae_engine::TimerRuntime;
use std::sync::Arc;

/// One event consumer per service, independent of client connections.
pub struct ServerServices {
    pub timer: Arc<TimerRuntime>,
    dispatcher: tokio::task::JoinHandle<()>,
    stop: tokio::sync::watch::Sender<bool>,
}
impl ServerServices {
    pub async fn start(timer: TimerRuntime, registry: Arc<Registry>) -> anyhow::Result<Self> {
        let timer = Arc::new(timer);
        let receiver = timer.watch().await?;
        let handle = timer.handle();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let dispatcher = tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    biased;
                    _ = stopping.changed() => break,
                    event = receiver.recv() => match event { Ok(event) => event, Err(_) => break }
                };
                if event.from_rt_id != "timer" { continue }
                let EventType::Any(id, data) = event.event_type else { continue };
                if id != "timer" { continue }
                let Ok(fired) = data.downcast::<TimerFired>() else { continue };
                if !registry.timer_capacity() { continue }
                let occurrence = match handle.claim(&fired).await {
                    Ok(Some(o)) => o,
                    Ok(None) => continue,
                    Err(e) => { eprintln!("timer claim: {e:#}"); continue }
                };
                if let Err(e) = registry.open_internal_agent(occurrence.clone()).await {
                    if e.code == "resource_exhausted" {
                        if let Err(error) = handle.defer(&occurrence.owner, &occurrence.occurrence_id).await { eprintln!("timer defer: {error:#}"); }
                    } else if let Err(error) = handle.acknowledge(&occurrence.owner, &occurrence.occurrence_id, TimerOccurrenceStatus::Failed, None, Some(format!("{}: {}", e.code, e.message))).await {
                        eprintln!("timer dispatch failure receipt: {error:#}");
                    }
                }
            }
        });
        Ok(Self { timer, dispatcher, stop })
    }
    pub async fn stop_dispatch(self) {
        self.timer.stop_dispatch().await;
        let _ = self.stop.send(true);
        let _ = self.dispatcher.await;
    }
}
