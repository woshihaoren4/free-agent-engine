use super::{
    connection::Outbound,
    session::{Actor, Command},
    workspace::WorkspaceHookBuilder,
};
use crate::protocol::*;
use fae_agent::SingleAgentPlanBuilder;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub kind: String,
    pub target: String,
    pub user: String,
    pub session: String,
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_waits_for_cleanup_after_command_receiver_closes() {
        let registry = Registry::new(PathBuf::from("."), "test".into());
        let key = Key {
            kind: "agent".into(),
            target: "worker".into(),
            user: "alice".into(),
            session: "timer-session".into(),
        };
        let (tx, rx) = mpsc::channel(1);
        let (done, closed) = tokio::sync::watch::channel(None);
        drop(rx);
        registry.state.lock().unwrap().entries.insert(
            key.clone(),
            Entry {
                timer: true,
                closed,
                key: key.clone(),
                open: OpenSession {
                    target: SessionTarget::Agent {
                        source: fae_agent::SingleAgentSource::AgentId("worker".into()),
                    },
                    workspace: PathBuf::from("."),
                    session_id: Some("timer-session".into()),
                    resume: None,
                    binding: None,
                },
                stream: "stream".into(),
                tx,
                leased: false,
                lease_id: String::new(),
                active: false,
                closing: true,
                disconnected: None,
            },
        );
        {
            let mut state = registry.state.lock().unwrap();
            let entry = state.entries.get_mut(&key).unwrap();
            entry.leased = true;
            entry.closing = false;
        }
        assert!(!registry.finish_round(&key));
        {
            let mut state = registry.state.lock().unwrap();
            let entry = state.entries.get_mut(&key).unwrap();
            assert!(!entry.closing);
            entry.leased = false;
            entry.timer = true;
        }
        assert!(registry.finish_round(&key));
        let r = registry.clone();
        let shutdown = tokio::spawn(async move { r.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        done.send(Some(String::new())).unwrap();
        shutdown.await.unwrap().unwrap();
    }
}
pub struct Entry {
    pub timer: bool,
    pub closed: tokio::sync::watch::Receiver<Option<String>>,
    pub key: Key,
    pub open: OpenSession,
    pub stream: String,
    pub tx: mpsc::Sender<Command>,
    pub leased: bool,
    pub lease_id: String,
    pub active: bool,
    pub closing: bool,
    pub disconnected: Option<Instant>,
}
#[derive(Default)]
pub struct State {
    pub entries: HashMap<Key, Entry>,
    pub cleaning: HashSet<(String, String)>,
    pub stopping: bool,
}
pub struct Registry {
    pub home: PathBuf,
    pub instance: String,
    pub state: Mutex<State>,
    pub timer: Option<fae_engine::TimerHandle>,
}
impl Registry {
    pub fn new(home: PathBuf, instance: String) -> Arc<Self> {
        Arc::new(Self {
            home,
            instance,
            state: Mutex::new(State::default()),
            timer: None,
        })
    }
    pub fn with_timer(
        home: PathBuf,
        instance: String,
        timer: fae_engine::TimerHandle,
    ) -> Arc<Self> {
        Arc::new(Self {
            home,
            instance,
            state: Mutex::new(State::default()),
            timer: Some(timer),
        })
    }
    pub fn counts(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (
            state.entries.len(),
            state.entries.values().filter(|e| e.active).count(),
        )
    }
    pub fn reserve_round(&self, key: &Key) -> Result<(), ProtocolError> {
        let mut state = self.state.lock().unwrap();
        if state.stopping || state.entries.values().filter(|e| e.active).count() >= 8 {
            return Err(ProtocolError::new(
                "resource_exhausted",
                "At most 8 rounds can run; service may be stopping",
            ));
        }
        if state
            .cleaning
            .contains(&(key.target.clone(), key.user.clone()))
        {
            return Err(ProtocolError::new(
                "cleanup_busy",
                "Agent/user data is being cleaned",
            ));
        }
        let entry = state
            .entries
            .get_mut(key)
            .ok_or_else(|| ProtocolError::new("resume_expired", "Session expired"))?;
        if entry.active {
            return Err(ProtocolError::new(
                "session_busy",
                "Wait for finished before starting another round",
            ));
        }
        entry.active = true;
        Ok(())
    }
    pub fn finish_round(&self, key: &Key) -> bool {
        if let Some(entry) = self.state.lock().unwrap().entries.get_mut(key) {
            let close = entry.timer && !entry.leased;
            entry.active = false;
            entry.timer = false;
            entry.closing |= close;
            if !entry.leased {
                entry.disconnected = Some(Instant::now());
            }
            return close;
        }
        false
    }
    pub fn remove(&self, key: &Key, stream: &str) {
        let mut state = self.state.lock().unwrap();
        if state
            .entries
            .get(key)
            .is_some_and(|entry| entry.stream == stream)
        {
            state.entries.remove(key);
        }
    }
    pub fn detach(&self, key: &Key, stream: &str, lease: &str) {
        if let Some(entry) = self
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut(key)
            .filter(|e| e.stream == stream && e.lease_id == lease)
        {
            entry.leased = false;
            entry.disconnected = Some(Instant::now());
        }
    }
    pub async fn open(
        self: &Arc<Self>,
        open: OpenSession,
        out: Outbound,
        request: String,
    ) -> Result<(Key, String, mpsc::Sender<Command>), ProtocolError> {
        if !open.workspace.is_absolute() || !open.workspace.is_dir() {
            return Err(ProtocolError::new(
                "invalid_workspace",
                "Workspace must be an absolute directory",
            ));
        }
        // Existing bindings use their fixed configuration, even after init --force
        // or deletion of the source files. Resume never reloads config or reruns work.
        let existing = {
            let mut state = self.state.lock().unwrap();
            if state.stopping {
                return Err(ProtocolError::new(
                    "resource_exhausted",
                    "Service is stopping",
                ));
            }
            let candidate = state.entries.values_mut().find(|entry| {
                open.resume.is_some()
                    && entry.open.binding == open.binding
                    && open
                        .binding
                        .as_ref()
                        .is_none_or(|b| b.user_id == entry.key.user)
                    && (open
                        .resume
                        .as_ref()
                        .is_some_and(|resume| resume.stream_id == entry.stream)
                        || (serde_json::to_value(&entry.open.target).unwrap()
                            == serde_json::to_value(&open.target).unwrap()
                            && match &open.session_id {
                                Some(id) => id == &entry.key.session,
                                None => {
                                    matches!(open.target, SessionTarget::Agent { .. })
                                        && entry.open.session_id.is_none()
                                }
                            }))
            });
            if let Some(entry) = candidate {
                if open
                    .resume
                    .as_ref()
                    .is_some_and(|resume| resume.stream_id != entry.stream)
                {
                    return Err(ProtocolError::new(
                        "resume_expired",
                        "Resume stream does not match this session",
                    ));
                }
                if entry.closing {
                    return Err(ProtocolError::new(
                        "resume_expired",
                        "Session is being released",
                    ));
                }
                if entry.open.workspace != open.workspace
                    || serde_json::to_value(&entry.open.target).unwrap()
                        != serde_json::to_value(&open.target).unwrap()
                    || open
                        .session_id
                        .as_ref()
                        .is_some_and(|id| *id != entry.key.session)
                {
                    return Err(ProtocolError::new(
                        "session_binding_conflict",
                        "Session binding cannot change",
                    ));
                }
                if entry.leased {
                    return Err(ProtocolError::new(
                        "session_in_use",
                        "Session already has a connected client",
                    ));
                }
                entry.leased = true;
                entry.lease_id = out.id.clone();
                entry.disconnected = None;
                Some((entry.key.clone(), entry.stream.clone(), entry.tx.clone()))
            } else {
                None
            }
        };
        if let Some((key, stream, tx)) = existing {
            let lease = out.id.clone();
            if tx
                .send(Command::Attach {
                    out,
                    request,
                    resume: open.resume,
                })
                .await
                .is_err()
            {
                self.detach(&key, &stream, &lease);
                return Err(ProtocolError::new(
                    "resume_expired",
                    "Session actor has expired",
                ));
            }
            return Ok((key, stream, tx));
        }
        if open.resume.is_some() {
            return Err(ProtocolError::new(
                "resume_expired",
                "Stream no longer exists; task was not rerun",
            ));
        }
        let mut builder = SingleAgentPlanBuilder::with_home_dir(&self.home);
        builder.set_timer_workspace(open.workspace.clone());
        builder.add_hook(WorkspaceHookBuilder::new(open.workspace.clone()));
        let (target, name, user, model, session, kind) = match &open.target {
            SessionTarget::Agent { source } => {
                builder
                    .freeze_source(source)
                    .await
                    .map_err(|e| ProtocolError::new("config_not_found", e))?;
                if let fae_agent::SingleAgentSource::Paths { config, prompt } = source {
                    if !config.is_absolute() || !prompt.is_absolute() {
                        return Err(ProtocolError::new(
                            "config_not_found",
                            "Config and prompt paths must be absolute",
                        ));
                    }
                }
                let (config, _) = builder
                    .load_config(source)
                    .await
                    .map_err(|e| ProtocolError::new("config_not_found", e))?;
                (
                    config.agent.name.clone(),
                    config.agent.name,
                    open.binding
                        .as_ref()
                        .map(|b| b.user_id.clone())
                        .unwrap_or(config.agent.user_id),
                    config.model.model,
                    open.session_id.clone().unwrap_or(config.agent.session_id),
                    "agent",
                )
            }
            SessionTarget::Workflow { workflow_id, .. } => {
                if workflow_id.is_empty()
                    || workflow_id.contains(['/', '\\'])
                    || workflow_id == ".."
                {
                    return Err(ProtocolError::new(
                        "invalid_session_id",
                        "Invalid workflow ID",
                    ));
                }
                (
                    workflow_id.clone(),
                    workflow_id.clone(),
                    fae_agent::DEFAULT_USER_ID.into(),
                    "workflow".into(),
                    open.session_id.clone().unwrap_or_else(wd_tools::uuid::v4),
                    "workflow",
                )
            }
        };
        fae_engine::SessionRuntime::with_host_dir(&self.home)
            .session_path(&target, &user, &session)
            .map_err(|e| ProtocolError::new("invalid_session_id", e))?;
        let key = Key {
            kind: kind.into(),
            target: target.clone(),
            user: user.clone(),
            session: session.clone(),
        };
        let (tx, stream, new_actor) = {
            let mut state = self.state.lock().unwrap();
            if state.stopping {
                return Err(ProtocolError::new(
                    "resource_exhausted",
                    "Service is stopping",
                ));
            }
            if let Some(entry) = state.entries.get_mut(&key) {
                if entry.closing {
                    return Err(ProtocolError::new(
                        "resume_expired",
                        "Session is being released",
                    ));
                }
                if entry.open.workspace != open.workspace
                    || entry.open.binding != open.binding
                    || serde_json::to_value(&entry.open.target).unwrap()
                        != serde_json::to_value(&open.target).unwrap()
                {
                    return Err(ProtocolError::new(
                        "session_binding_conflict",
                        "Session is bound to another workspace or source",
                    ));
                }
                if entry.leased {
                    return Err(ProtocolError::new(
                        "session_in_use",
                        "Session already has a connected client",
                    ));
                }
                if open
                    .resume
                    .as_ref()
                    .is_some_and(|r| r.stream_id != entry.stream)
                {
                    return Err(ProtocolError::new(
                        "resume_expired",
                        "Stream belongs to an expired instance",
                    ));
                }
                entry.leased = true;
                entry.lease_id = out.id.clone();
                entry.disconnected = None;
                (entry.tx.clone(), entry.stream.clone(), None)
            } else {
                if open.resume.is_some() {
                    return Err(ProtocolError::new(
                        "resume_expired",
                        "Stream no longer exists; workflow was not restarted",
                    ));
                }
                if state.entries.len() >= 32 {
                    return Err(ProtocolError::new(
                        "resource_exhausted",
                        "At most 32 sessions may be retained",
                    ));
                }
                let stream = wd_tools::uuid::v4();
                let (tx, rx) = mpsc::channel(512);
                let (done, closed) = tokio::sync::watch::channel(None);
                state.entries.insert(
                    key.clone(),
                    Entry {
                        timer: false,
                        closed,
                        key: key.clone(),
                        open: open.clone(),
                        stream: stream.clone(),
                        tx: tx.clone(),
                        leased: true,
                        lease_id: out.id.clone(),
                        active: false,
                        closing: false,
                        disconnected: None,
                    },
                );
                (tx, stream, Some((rx, done)))
            }
        };
        if let Some((rx, done)) = new_actor {
            let info = SessionInfo {
                target_id: target,
                name,
                user_id: user,
                model,
                session_id: session,
                workspace: open.workspace.clone(),
                status: SessionStatus {
                    state: "idle".into(),
                    round_id: None,
                    latest_seq: 0,
                },
                resumed: false,
            };
            let actor = Actor::new(
                self.clone(),
                key.clone(),
                stream.clone(),
                open.target.clone(),
                info,
                builder,
                tx.clone(),
                done,
            );
            tokio::spawn(actor.run(rx));
        }
        let lease = out.id.clone();
        if tx
            .send(Command::Attach {
                out,
                request,
                resume: open.resume,
            })
            .await
            .is_err()
        {
            self.detach(&key, &stream, &lease);
            return Err(ProtocolError::new("internal_error", "Session actor exited"));
        }
        Ok((key, stream, tx))
    }
    pub fn timer_capacity(&self) -> bool {
        let state = self.state.lock().unwrap();
        !state.stopping
            && state.entries.len() < 32
            && state.entries.values().filter(|e| e.active).count() < 8
            && state
                .entries
                .values()
                .filter(|e| e.active && e.timer)
                .count()
                < fae_engine::TIMER_CONCURRENT_LIMIT
    }

    /// This entry point takes persisted ownership, never configuration defaults.
    pub async fn open_internal_agent(
        self: &Arc<Self>,
        occurrence: fae_agent::TimerOccurrence,
    ) -> Result<String, ProtocolError> {
        let binding = &occurrence.execution_binding;
        if binding.owner != occurrence.owner
            || !binding.workspace.is_absolute()
            || !binding.workspace.is_dir()
        {
            return Err(ProtocolError::new(
                "invalid_workspace",
                "Timer execution binding is invalid",
            ));
        }
        if let fae_agent::SingleAgentSource::Paths { config, prompt } = &binding.source {
            if !config.is_absolute() || !prompt.is_absolute() {
                return Err(ProtocolError::new(
                    "config_not_found",
                    "Timer source paths must be absolute",
                ));
            }
        }
        let mut builder = SingleAgentPlanBuilder::with_home_dir(&self.home);
        builder.add_hook(WorkspaceHookBuilder::new(binding.workspace.clone()));
        builder.set_timer_workspace(binding.workspace.clone());
        builder
            .freeze_source(&binding.source)
            .await
            .map_err(|e| ProtocolError::new("config_not_found", e))?;
        let (config, _) = builder
            .load_config(&binding.source)
            .await
            .map_err(|e| ProtocolError::new("config_not_found", e))?;
        if config.agent.name != occurrence.owner.agent_id {
            return Err(ProtocolError::new(
                "agent_binding_conflict",
                "Timer agent was renamed",
            ));
        }
        fae_engine::SessionRuntime::with_host_dir(&self.home)
            .session_path(
                &occurrence.owner.agent_id,
                &occurrence.owner.user_id,
                &occurrence.session_id,
            )
            .map_err(|e| ProtocolError::new("invalid_session_id", e))?;
        let key = Key {
            kind: "agent".into(),
            target: occurrence.owner.agent_id.clone(),
            user: occurrence.owner.user_id.clone(),
            session: occurrence.session_id.clone(),
        };
        let target = SessionTarget::Agent {
            source: binding.source.clone(),
        };
        let open = OpenSession {
            target: target.clone(),
            workspace: binding.workspace.clone(),
            session_id: Some(key.session.clone()),
            resume: None,
            binding: None,
        };
        let stream = wd_tools::uuid::v4();
        let (tx, rx) = mpsc::channel(512);
        let (done, closed) = tokio::sync::watch::channel(None);
        {
            let mut state = self.state.lock().unwrap();
            if state.stopping
                || state.entries.len() >= 32
                || state.entries.values().filter(|e| e.active).count() >= 8
                || state
                    .entries
                    .values()
                    .filter(|e| e.active && e.timer)
                    .count()
                    >= fae_engine::TIMER_CONCURRENT_LIMIT
                || state
                    .cleaning
                    .contains(&(key.target.clone(), key.user.clone()))
            {
                return Err(ProtocolError::new(
                    "resource_exhausted",
                    "Timer is waiting for service capacity",
                ));
            }
            if state.entries.contains_key(&key) {
                return Err(ProtocolError::new(
                    "session_binding_conflict",
                    "Timer session already exists",
                ));
            }
            state.entries.insert(
                key.clone(),
                Entry {
                    timer: true,
                    closed,
                    key: key.clone(),
                    open,
                    stream: stream.clone(),
                    tx: tx.clone(),
                    leased: false,
                    lease_id: String::new(),
                    active: true,
                    closing: false,
                    disconnected: None,
                },
            );
        }
        let info = SessionInfo {
            target_id: key.target.clone(),
            name: config.agent.name,
            user_id: key.user.clone(),
            model: config.model.model,
            session_id: key.session.clone(),
            workspace: binding.workspace.clone(),
            status: SessionStatus {
                state: "idle".into(),
                round_id: None,
                latest_seq: 0,
            },
            resumed: false,
        };
        let mut actor = Actor::new(
            self.clone(),
            key,
            stream,
            target,
            info,
            builder,
            tx.clone(),
            done,
        );
        actor.reserve_internal_round();
        tokio::spawn(actor.run(rx));
        let (reply, result) = oneshot::channel();
        tx.send(Command::InternalStart {
            input: occurrence.input.clone(),
            occurrence,
            reply,
        })
        .await
        .map_err(|e| ProtocolError::new("internal_error", e))?;
        result
            .await
            .map_err(|e| ProtocolError::new("internal_error", e))?
    }
    pub fn begin_clean(&self, key: &Key) -> Result<(), ProtocolError> {
        let mut state = self.state.lock().unwrap();
        let group = (key.target.clone(), key.user.clone());
        if state.cleaning.contains(&group)
            || state
                .entries
                .values()
                .any(|e| e.key.target == key.target && e.key.user == key.user && e.active)
        {
            return Err(ProtocolError::new(
                "cleanup_busy",
                "Agent/user has active rounds",
            ));
        }
        state.cleaning.insert(group);
        Ok(())
    }
    pub fn end_clean(&self, key: &Key) {
        self.state
            .lock()
            .unwrap()
            .cleaning
            .remove(&(key.target.clone(), key.user.clone()));
    }
    pub async fn reap(&self) {
        let expired = {
            let mut state = self.state.lock().unwrap();
            let keys: Vec<_> = state
                .entries
                .iter()
                .filter(|(_, e)| {
                    !e.leased
                        && !e.active
                        && !e.closing
                        && e.disconnected
                            .is_some_and(|t| t.elapsed() >= Duration::from_secs(1800))
                })
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter()
                .filter_map(|k| {
                    let entry = state.entries.get_mut(&k)?;
                    entry.closing = true;
                    Some(entry.tx.clone())
                })
                .collect::<Vec<_>>()
        };
        for tx in expired {
            let _ = tx.send(Command::Stop(None)).await;
        }
    }
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let entries = {
            let mut state = self.state.lock().unwrap();
            state.stopping = true;
            state
                .entries
                .values()
                .map(|e| (e.tx.clone(), e.closed.clone()))
                .collect::<Vec<_>>()
        };
        let mut completions = Vec::new();
        for (tx, closed) in entries {
            // A timer actor may already be leaving after its terminal receipt.
            // Its completion signal also covers Stop queued after the run loop.
            let _ = tx.send(Command::Stop(None)).await;
            completions.push(closed);
        }
        for mut closed in completions {
            loop {
                if let Some(error) = closed.borrow().clone() {
                    anyhow::ensure!(error.is_empty(), "session cleanup failed: {error}");
                    break;
                }
                closed.changed().await?;
            }
        }
        Ok(())
    }
}
