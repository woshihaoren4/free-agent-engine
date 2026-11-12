use crate::timer::{self, Partition, Store};
use fae_agent::*;
use fs2::FileExt;
use std::{path::PathBuf, sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}}, time::Duration};
use tokio::sync::{Mutex, Notify};
use wd_tools::channel::{Channel, Receiver, Sender};

pub const TIMER_PENDING_LIMIT: usize = 256;
pub const TIMER_CONCURRENT_LIMIT: usize = 2;

#[derive(Debug)]
struct Inner {
    home: PathBuf,
    store: Mutex<Store>,
    resolver: Arc<dyn TimerExecutionBindingResolver>,
    wake: Notify,
    stopping: AtomicBool,
    next_owner: AtomicUsize,
    // Shared across all clients; released only when every handle is dropped.
    _lock: std::fs::File,
}
#[derive(Debug, Clone)]
pub struct TimerHandle(Arc<Inner>);
#[derive(Debug)]
pub struct TimerRuntime {
    handle: TimerHandle,
    receiver: Mutex<Option<Receiver<Event>>>,
    sender: Sender<Event>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
#[derive(Debug, Clone)]
pub struct TimerClientRuntime { handle: TimerHandle }

impl TimerRuntime {
    pub async fn with_home_dir(home: impl Into<PathBuf>) -> anyhow::Result<Self> {
        Self::with_resolver(home, Arc::new(ScopedTimerBindingResolver)).await
    }
    pub async fn with_resolver(home: impl Into<PathBuf>, resolver: Arc<dyn TimerExecutionBindingResolver>) -> anyhow::Result<Self> {
        let home = home.into();
        let home_copy = home.clone();
        let (lock, store) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let schedule = home_copy.join("schedule");
            std::fs::create_dir_all(&schedule)?;
            let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(schedule.join("timer.lock"))?;
            lock.try_lock_exclusive().map_err(|e| anyhow::anyhow!("timer store already in use: {e}"))?;
            Ok((lock, timer::load(&home_copy)?))
        }).await??;
        let (sender, receiver) = Channel::new(TIMER_PENDING_LIMIT);
        let handle = TimerHandle(Arc::new(Inner { home, store: Mutex::new(store), resolver, wake: Notify::new(), stopping: AtomicBool::new(false), next_owner: AtomicUsize::new(0), _lock: lock }));
        handle.recover().await?;
        Ok(Self { handle, receiver: Mutex::new(Some(receiver)), sender, task: Mutex::new(None) })
    }
    pub fn handle(&self) -> TimerHandle { self.handle.clone() }
    pub async fn stop_dispatch(&self) {
        self.handle.0.stopping.store(true, Ordering::SeqCst);
        self.handle.0.wake.notify_waiters();
        if let Some(task) = self.task.lock().await.take() { let _ = task.await; }
    }
}

impl TimerHandle {
    pub fn client(&self) -> TimerClientRuntime { TimerClientRuntime { handle: self.clone() } }
    async fn save(&self, store: &mut Store, key: timer::PartitionKey, partition: Partition) -> anyhow::Result<()> {
        timer::commit(self.0.home.clone(), key.clone(), partition.clone()).await?;
        store.partitions.insert(key, partition);
        Ok(())
    }
    async fn recover(&self) -> anyhow::Result<()> {
        let mut store = self.0.store.lock().await;
        for key in store.partitions.keys().cloned().collect::<Vec<_>>() {
            let mut p = store.partitions[&key].clone();
            let mut changed = false;
            for o in &mut p.occurrences {
                if matches!(o.status, TimerOccurrenceStatus::Dispatching | TimerOccurrenceStatus::Running) {
                    o.status = TimerOccurrenceStatus::Interrupted;
                    o.completed_at = Some(chrono::Utc::now().to_rfc3339());
                    o.error = Some("service interrupted; execution is not replayed".into());
                    changed = true;
                }
            }
            if changed { self.save(&mut store, key, p).await?; }
        }
        Ok(())
    }
    pub async fn request(&self, ctx: &Ctx, request: TimerRequest) -> anyhow::Result<TimerResponse> {
        let handle = self.clone();
        let ctx = ctx.clone();
        // A cancelled caller must not cancel an in-flight disk transaction.
        tokio::spawn(async move { handle.request_inner(&ctx, request).await }).await?
    }
    async fn request_inner(&self, ctx: &Ctx, mut request: TimerRequest) -> anyhow::Result<TimerResponse> {
        anyhow::ensure!(!self.0.stopping.load(Ordering::SeqCst), "timer_unavailable: service stopping");
        let scope = ctx.scope()?;
        let owner = TimerOwner { agent_id: scope.agent_id.clone().ok_or_else(|| anyhow::anyhow!("execution_binding_unavailable: missing agent"))?, user_id: scope.user.user_id.clone() };
        let key = timer::key(&owner)?;
        let now = chrono::Utc::now();
        let now_text = now.to_rfc3339();
        if let TimerRequest::Create { schedule, .. } = &mut request { schedule.set_cron(timer::parse(schedule)?.0.to_string()); }
        let fingerprint = serde_json::to_string(&request)?;
        let mut store = self.0.store.lock().await;
        anyhow::ensure!(!store.corrupt.contains(&key), "timer_store_corrupt");
        let mut p = store.partitions.get(&key).cloned().unwrap_or_default();
        if let TimerRequest::Query { id, state, include_deleted, cursor, include_occurrences, limit } = request {
            anyhow::ensure!(limit > 0 && limit <= 100, "invalid_arguments: limit must be 1..100");
            let offset = cursor.as_deref().unwrap_or("0").parse::<usize>().map_err(|_| anyhow::anyhow!("invalid_arguments: cursor"))?;
            if let Some(id) = &id {
                anyhow::ensure!(p.timers.iter().any(|t| &t.id == id), "timer_not_found");
            }
            let matches: Vec<_> = p.timers.iter().filter(|t| id.as_ref().is_none_or(|id| id == &t.id) && (include_deleted || t.state != TimerState::Deleted) && state.is_none_or(|s| s == t.state)).collect();
            let timers = matches.iter().skip(offset).take(limit).map(|t| (*t).clone()).collect();
            let mut result = response(timers, &p)?;
            if !include_occurrences { result.occurrences.clear(); }
            result.next_cursor = (offset.saturating_add(limit) < matches.len()).then(|| offset.saturating_add(limit).to_string());
            return Ok(result);
        }
        let id = match request {
            TimerRequest::Create { idempotency_key, name, input, schedule, misfire_policy } => {
                anyhow::ensure!(!idempotency_key.trim().is_empty() && idempotency_key.len() <= 256, "invalid_arguments: idempotency_key must contain 1..256 bytes");
                if let Some((original, id)) = p.idempotency.get(&idempotency_key) {
                    anyhow::ensure!(original == &fingerprint, "idempotency_conflict");
                    return response(p.timers.iter().filter(|t| t.id == *id).cloned().collect(), &p);
                }
                validate_text(&name, &input)?;
                let binding = self.0.resolver.resolve(ctx)?;
                anyhow::ensure!(binding.owner == owner && binding.workspace.is_absolute() && binding.workspace.is_dir(), "execution_binding_unavailable");
                let session = scope.session.as_ref().ok_or_else(|| anyhow::anyhow!("execution_binding_unavailable: missing session"))?;
                let total: usize = store.partitions.values().map(|p| p.timers.iter().filter(|t| t.state != TimerState::Deleted).count()).sum();
                anyhow::ensure!(total < 1000 && p.timers.iter().filter(|t| t.state != TimerState::Deleted).count() < 100, "resource_exhausted");
                let id = wd_tools::uuid::v4();
                let next = timer::next(&schedule, now)?.map(|t| t.to_rfc3339());
                anyhow::ensure!(next.is_some(), "no_future_occurrence");
                p.timers.push(TimerDefinition { id: id.clone(), revision: 1, owner, created_session_id: session.session_id.clone(), execution_binding: binding, name, input, schedule, misfire_policy, state: TimerState::Active, completion_reason: None, accepted_count: 0, next_fire_at: next, queued_at: None, last_scheduled_at: None, created_at: now_text.clone(), updated_at: now_text.clone(), last_occurrence_id: None });
                p.idempotency.insert(idempotency_key, (fingerprint, id.clone()));
                id
            }
            TimerRequest::Update { id, expected_revision, name, input, mut schedule, misfire_policy, enabled } => {
                anyhow::ensure!(name.is_some() || input.is_some() || schedule.is_some() || misfire_policy.is_some() || enabled.is_some(), "invalid_arguments: update is empty");
                let t = p.timers.iter_mut().find(|t| t.id == id).ok_or_else(|| anyhow::anyhow!("timer_not_found"))?;
                anyhow::ensure!(t.revision == expected_revision, "revision_conflict");
                anyhow::ensure!(!matches!(t.state, TimerState::Completed | TimerState::Deleted), "timer_completed");
                if let Some(enabled) = enabled {
                    t.state = if enabled { TimerState::Active } else { TimerState::Paused };
                }
                if let Some(name) = name { t.name = name; }
                if let Some(input) = input { t.input = input; }
                validate_text(&t.name, &t.input)?;
                if let Some(s) = &mut schedule {
                    anyhow::ensure!(s.limit().is_none_or(|n| n >= t.accepted_count), "invalid_schedule: limit below accepted_count");
                    // Reaching the total does not require another future date.
                    if s.limit() != Some(t.accepted_count) { normalize(s, now)?; } else { s.set_cron(timer::parse(s)?.0.to_string()); }
                }
                let reschedule = schedule.is_some() || enabled.is_some();
                if let Some(s) = schedule { t.schedule = s; }
                if let Some(policy) = misfire_policy { t.misfire_policy = policy; }
                if t.schedule.limit() == Some(t.accepted_count) {
                    t.state = TimerState::Completed;
                    t.completion_reason = Some("max_fires_reached".into());
                }
                if reschedule {
                    t.queued_at = None;
                    t.next_fire_at = if t.state == TimerState::Active { timer::next(&t.schedule, now)?.map(|t| t.to_rfc3339()) } else { None };
                    if t.state == TimerState::Active && t.next_fire_at.is_none() { t.state = TimerState::Completed; t.completion_reason = Some("no_future_occurrence".into()); }
                }
                t.revision += 1;
                t.updated_at = now_text.clone();
                id
            }
            TimerRequest::Delete { id, expected_revision } => {
                let t = p.timers.iter_mut().find(|t| t.id == id).ok_or_else(|| anyhow::anyhow!("timer_not_found"))?;
                if t.state != TimerState::Deleted {
                    anyhow::ensure!(expected_revision.is_none_or(|v| t.revision == v), "revision_conflict");
                    t.state = TimerState::Deleted;
                    t.next_fire_at = None; t.queued_at = None;
                    t.revision += 1; t.updated_at = now_text.clone();
                    for o in &mut p.occurrences {
                        if o.timer_id == id && o.status == TimerOccurrenceStatus::Pending {
                            o.status = TimerOccurrenceStatus::Suppressed;
                            o.completed_at = Some(now_text.clone());
                        }
                    }
                }
                id
            }
            TimerRequest::Query { .. } => unreachable!(),
        };
        let result = response(p.timers.iter().filter(|t| t.id == id).cloned().collect(), &p)?;
        self.save(&mut store, key, p).await?;
        self.0.wake.notify_one();
        Ok(result)
    }

    /// Atomically claim a pending occurrence. Duplicate event delivery is harmless.
    pub async fn claim(&self, fired: &TimerFired) -> anyhow::Result<Option<TimerOccurrence>> {
        let handle = self.clone();
        let fired = fired.clone();
        tokio::spawn(async move { handle.claim_inner(&fired).await }).await?
    }
    async fn claim_inner(&self, fired: &TimerFired) -> anyhow::Result<Option<TimerOccurrence>> {
        if self.0.stopping.load(Ordering::SeqCst) { return Ok(None) }
        let key = timer::key(&fired.owner)?;
        let mut store = self.0.store.lock().await;
        let Some(mut p) = store.partitions.get(&key).cloned() else { return Ok(None) };
        let Some(o) = p.occurrences.iter_mut().find(|o| o.occurrence_id == fired.occurrence_id && o.timer_id == fired.timer_id && o.session_id == fired.session_id && o.definition_revision == fired.definition_revision && o.status == TimerOccurrenceStatus::Pending) else { return Ok(None) };
        o.status = TimerOccurrenceStatus::Dispatching;
        let result = o.clone();
        self.save(&mut store, key, p).await?;
        Ok(Some(result))
    }
    pub async fn acknowledge(&self, owner: &TimerOwner, id: &str, status: TimerOccurrenceStatus, round: Option<String>, error: Option<String>) -> anyhow::Result<()> {
        let handle = self.clone();
        let owner = owner.clone();
        let id = id.to_owned();
        tokio::spawn(async move { handle.acknowledge_inner(&owner, &id, status, round, error).await }).await?
    }
    async fn acknowledge_inner(&self, owner: &TimerOwner, id: &str, status: TimerOccurrenceStatus, round: Option<String>, error: Option<String>) -> anyhow::Result<()> {
        let key = timer::key(owner)?;
        let mut store = self.0.store.lock().await;
        let mut p = store.partitions.get(&key).cloned().ok_or_else(|| anyhow::anyhow!("timer_not_found"))?;
        let o = p.occurrences.iter_mut().find(|o| o.occurrence_id == id).ok_or_else(|| anyhow::anyhow!("timer_not_found"))?;
        if o.status.terminal() { return Ok(()) }
        anyhow::ensure!((status == TimerOccurrenceStatus::Running && o.status == TimerOccurrenceStatus::Dispatching) || (status.terminal() && matches!(o.status, TimerOccurrenceStatus::Dispatching | TimerOccurrenceStatus::Running)), "invalid occurrence transition");
        o.status = status;
        if status == TimerOccurrenceStatus::Running { o.started_at = Some(chrono::Utc::now().to_rfc3339()); }
        if status.terminal() { o.completed_at = Some(chrono::Utc::now().to_rfc3339()); }
        if round.is_some() { o.round_id = round; }
        o.error = error;
        self.save(&mut store, key, p).await?;
        self.0.wake.notify_one();
        Ok(())
    }
    /// No actor was created; release a claim made before capacity changed.
    pub async fn defer(&self, owner: &TimerOwner, id: &str) -> anyhow::Result<()> {
        let handle = self.clone();
        let owner = owner.clone();
        let id = id.to_owned();
        tokio::spawn(async move { handle.defer_inner(&owner, &id).await }).await?
    }
    async fn defer_inner(&self, owner: &TimerOwner, id: &str) -> anyhow::Result<()> {
        let key = timer::key(owner)?;
        let mut store = self.0.store.lock().await;
        let mut p = store.partitions.get(&key).cloned().ok_or_else(|| anyhow::anyhow!("timer_not_found"))?;
        let o = p.occurrences.iter_mut().find(|o| o.occurrence_id == id).ok_or_else(|| anyhow::anyhow!("timer_not_found"))?;
        anyhow::ensure!(o.status == TimerOccurrenceStatus::Dispatching, "invalid occurrence transition");
        o.status = TimerOccurrenceStatus::Pending;
        self.save(&mut store, key, p).await
    }
    async fn poll(&self, now: chrono::DateTime<chrono::Utc>) -> anyhow::Result<Vec<TimerFired>> {
        let mut store = self.0.store.lock().await;
        let mut pending = store.partitions.values().flat_map(|p| &p.occurrences).filter(|o| o.status == TimerOccurrenceStatus::Pending).count();
        let mut keys = store.partitions.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        if !keys.is_empty() {
            let offset = self.0.next_owner.fetch_add(1, Ordering::Relaxed) % keys.len();
            keys.rotate_left(offset);
        }
        for key in keys.clone() {
            let mut p = store.partitions[&key].clone();
            let before = serde_json::to_vec(&p)?;
            for t in &mut p.timers {
                if t.state != TimerState::Active { continue }
                if let Some(due) = t.next_fire_at.as_deref().map(timer::timestamp).transpose()? {
                    if due <= now {
                        let misfire = now.signed_duration_since(due) > chrono::Duration::seconds(2);
                        if !(misfire && t.misfire_policy == TimerMisfirePolicy::Skip) {
                            let latest = timer::latest(&t.schedule, now)?.unwrap_or(due).max(due);
                            if t.last_scheduled_at.as_ref().is_none_or(|old| timer::timestamp(old).is_ok_and(|old| latest > old))
                                && t.queued_at.as_ref().is_none_or(|old| timer::timestamp(old).is_ok_and(|old| latest > old)) {
                                t.queued_at = Some(latest.to_rfc3339());
                            }
                        } else if t.schedule.limit() == Some(1) {
                            t.state = TimerState::Completed;
                            t.completion_reason = Some("expired_skipped".into());
                        }
                        t.next_fire_at = timer::next(&t.schedule, now)?.map(|v| v.to_rfc3339());
                    }
                }
                let busy = p.occurrences.iter().any(|o| o.timer_id == t.id && !o.status.terminal());
                if !busy && pending < TIMER_PENDING_LIMIT && let Some(scheduled_at) = t.queued_at.take() {
                    let o = TimerOccurrence { occurrence_id: wd_tools::uuid::v4(), timer_id: t.id.clone(), definition_revision: t.revision, owner: t.owner.clone(), execution_binding: t.execution_binding.clone(), input: t.input.clone(), session_id: wd_tools::uuid::v4(), scheduled_at: scheduled_at.clone(), emitted_at: now.to_rfc3339(), status: TimerOccurrenceStatus::Pending, started_at: None, completed_at: None, round_id: None, error: None };
                    t.accepted_count += 1;
                    t.last_scheduled_at = Some(scheduled_at);
                    t.last_occurrence_id = Some(o.occurrence_id.clone());
                    t.updated_at = now.to_rfc3339();
                    p.occurrences.push(o);
                    pending += 1;
                    if t.schedule.limit().is_some_and(|n| t.accepted_count >= n) {
                        t.state = TimerState::Completed; t.next_fire_at = None;
                        t.completion_reason = Some("max_fires_reached".into());
                    }
                }
                if t.next_fire_at.is_none() && t.queued_at.is_none() && t.state == TimerState::Active {
                    t.state = TimerState::Completed; t.completion_reason = Some("no_future_occurrence".into());
                }
            }
            let cutoff = now - chrono::Duration::days(30);
            let mut terminal_count = 0;
            p.occurrences.reverse();
            p.occurrences.retain(|o| {
                if !o.status.terminal() { return true }
                terminal_count += 1;
                terminal_count <= 1000 && o.completed_at.as_deref().and_then(|v| timer::timestamp(v).ok()).is_none_or(|v| v >= cutoff)
            });
            p.occurrences.reverse();
            if serde_json::to_vec(&p)? != before { self.save(&mut store, key, p).await?; }
        }
        let mut queues: Vec<_> = keys.iter().map(|key| {
            let mut events: Vec<_> = store.partitions[key].occurrences.iter().filter(|o| o.status == TimerOccurrenceStatus::Pending).map(TimerFired::from).collect();
            events.sort_by(|a, b| a.scheduled_at.cmp(&b.scheduled_at));
            std::collections::VecDeque::from(events)
        }).collect();
        let mut events = vec![];
        loop {
            let mut any = false;
            for queue in &mut queues {
                if let Some(event) = queue.pop_front() { events.push(event); any = true; }
            }
            if !any { break }
        }
        Ok(events)
    }
}
fn normalize(schedule: &mut TimerSchedule, now: chrono::DateTime<chrono::Utc>) -> anyhow::Result<()> {
    schedule.set_cron(timer::parse(schedule)?.0.to_string());
    anyhow::ensure!(timer::next(schedule, now)?.is_some(), "no_future_occurrence");
    Ok(())
}
fn validate_text(name: &str, input: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!name.trim().is_empty() && name.len() <= 256 && !input.trim().is_empty() && input.len() <= 65536, "invalid_arguments: name or input empty or too long");
    Ok(())
}
fn response(timers: Vec<TimerDefinition>, p: &Partition) -> anyhow::Result<TimerResponse> {
    let mut next_local = vec![];
    let mut preview = vec![];
    for t in &timers {
        if let Some(at) = &t.next_fire_at {
            let (_, tz) = timer::parse(&t.schedule)?;
            let mut at = timer::timestamp(at)?;
            next_local.push(at.with_timezone(&tz).to_rfc3339());
            let count = t.schedule.limit().map(|n| n.saturating_sub(t.accepted_count).min(3)).unwrap_or(3);
            for _ in 0..count {
                preview.push(at.with_timezone(&tz).to_rfc3339());
                let Some(next) = timer::next(&t.schedule, at)? else { break };
                at = next;
            }
        }
    }
    let occurrences = timers.iter().flat_map(|t| p.occurrences.iter().rev().filter(|o| o.timer_id == t.id).take(10).cloned()).collect();
    Ok(TimerResponse { timers, occurrences, next_local, preview, next_cursor: None })
}
#[async_trait::async_trait]
impl Runtime for TimerRuntime {
    fn id(&self) -> &str { "timer" }
    async fn exec(&self, task: &mut TaskRequest) -> fae_agent::Result<TaskResponse> {
        if task.meta.executor != "timer" { return Err(fae_agent::Error::RuntimeNoSupport) }
        let task = TaskReq::<TimerRequest>::try_from_request(task).ok_or(fae_agent::Error::RuntimeNoSupport)?;
        Ok(RuntimeSelectExec::exec(&self.handle.client(), task).await?.into_response())
    }
    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        let receiver = self.receiver.lock().await.take().ok_or_else(|| anyhow::anyhow!("timer watch already registered"))?;
        let handle = self.handle.clone();
        let sender = self.sender.clone();
        *self.task.lock().await = Some(tokio::spawn(async move {
            while !handle.0.stopping.load(Ordering::SeqCst) {
                match handle.poll(chrono::Utc::now()).await {
                    Ok(events) => for fired in events {
                        if handle.0.stopping.load(Ordering::SeqCst) { break }
                        let event = Event { from_rt_id: "timer".into(), event_type: EventType::Any("timer".into(), Box::new(fired)) };
                        if !matches!(tokio::time::timeout(Duration::from_millis(50), sender.send(event)).await, Ok(Ok(()))) { break }
                    },
                    Err(e) => eprintln!("timer scheduler: {e:#}"),
                }
                tokio::select! { _ = handle.0.wake.notified() => {}, _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
            }
        }));
        Ok(receiver)
    }
    async fn exit(&self) -> fae_agent::Result<()> { self.stop_dispatch().await; Ok(()) }
}
#[async_trait::async_trait]
impl RuntimeSelectExec<TimerRequest, TimerResponse, TimerQuery, TimerList> for TimerClientRuntime {
    fn id(&self) -> &str { "timer" }
    fn tys(&self) -> Vec<TaskType> { vec![TaskType::Any("timer".into())] }
    async fn exec(&self, task: TaskReq<TimerRequest>) -> fae_agent::Result<TaskResp<TimerResponse>> {
        let resp = self.handle.request(&task.ctx, task.req).await?;
        Ok(TaskResp { ctx: task.ctx, meta: task.meta, resp })
    }
}

#[cfg(test)]
#[path = "../timer/tests.rs"]
mod tests;
