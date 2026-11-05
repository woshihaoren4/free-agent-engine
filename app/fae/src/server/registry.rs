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
pub struct Entry {
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
}
impl Registry {
    pub fn new(home: PathBuf, instance: String) -> Arc<Self> {
        Arc::new(Self {
            home,
            instance,
            state: Mutex::new(State::default()),
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
    pub fn finish_round(&self, key: &Key) {
        if let Some(entry) = self.state.lock().unwrap().entries.get_mut(key) {
            entry.active = false;
            if !entry.leased {
                entry.disconnected = Some(Instant::now());
            }
        }
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
                open.resume
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
                        })
            });
            if let Some(entry) = candidate {
                if open.resume.as_ref().is_some_and(|resume| resume.stream_id != entry.stream) {
                    return Err(ProtocolError::new("resume_expired", "Resume stream does not match this session"));
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
                    config.agent.user_id,
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
                state.entries.insert(
                    key.clone(),
                    Entry {
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
                (tx, stream, Some(rx))
            }
        };
        if let Some(rx) = new_actor {
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
                .map(|e| e.tx.clone())
                .collect::<Vec<_>>()
        };
        let mut replies = Vec::new();
        for tx in entries {
            let (done, rx) = oneshot::channel();
            tx.send(Command::Stop(Some(done))).await?;
            replies.push(rx);
        }
        for reply in replies {
            reply.await??;
        }
        Ok(())
    }
}
