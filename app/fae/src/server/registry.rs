use super::{
    connection::Outbound,
    session::{Actor, Command},
    workspace::SessionWorkspace,
};
use crate::{
    protocol::ProtocolError,
    session_host::{DefinitionTarget, LarkPrincipal, SessionFacts, StoredSessionSpec},
};
use fae_agent::{
    PlanTarget, SingleAgentPlanBuilder, SingleAgentSource,
    protocol::{Message, RequestPayload},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StorageBinding {
    pub kind: String,
    pub target: String,
    pub user: String,
    pub session: String,
}
pub type Key = StorageBinding;

impl StorageBinding {
    pub fn logical(&self) -> fae_agent::SessionKey {
        fae_agent::SessionKey {
            user_id: fae_agent::UserId::new(self.user.clone()).expect("validated user"),
            session_id: fae_agent::SessionId::new(self.session.clone()).expect("validated session"),
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredLarkBinding {
    open: StoredSessionSpec,
    target_id: String,
}
pub struct Entry {
    pub handle: Option<fae_agent::SessionHandle>,
    pub timer: bool,
    pub closed: tokio::sync::watch::Receiver<Option<String>>,
    pub key: Key,
    pub open: StoredSessionSpec,
    pub stream: String,
    pub tx: mpsc::Sender<Command>,
    pub leased: bool,
    pub leases: HashSet<String>,
    pub pending_admission: bool,
    pub admission_target: Option<String>,
    pub closing: bool,
    pub disconnected: Option<Instant>,
}
impl Entry {
    fn active(&self) -> bool {
        self.pending_admission
            || self
                .handle
                .as_ref()
                .is_some_and(|handle| handle.read_summary().active_execution.is_some())
    }
    fn uses_target(&self, target: &str) -> bool {
        self.admission_target.as_deref() == Some(target)
            || self
                .handle
                .as_ref()
                .and_then(|handle| handle.execution(None).ok())
                .is_some_and(|execution| match execution.target {
                    PlanTarget::Agent { agent_id } => agent_id == target,
                    PlanTarget::Workflow { workflow_id } => workflow_id == target,
                })
    }
}
#[derive(Default)]
pub struct State {
    pub entries: HashMap<fae_agent::SessionKey, Entry>,
    pub cleaning: HashSet<(String, String)>,
    pub stopping: bool,
}
pub struct Registry {
    requests: Mutex<HashMap<String, Arc<tokio::sync::Mutex<super::replay::Requests>>>>,
    pub home: PathBuf,
    pub instance: String,
    pub state: Mutex<State>,
    pub timer: Option<fae_engine::TimerHandle>,
    pub plugins: Arc<fae_agent::plugin::PluginManager>,
}
impl Registry {
    pub fn requests_for(&self, user: &str) -> Arc<tokio::sync::Mutex<super::replay::Requests>> {
        self.requests
            .lock()
            .unwrap()
            .entry(user.into())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(super::replay::Requests::new())))
            .clone()
    }
    pub fn bind_handle(&self, key: &Key, handle: fae_agent::SessionHandle) {
        if let Some(entry) = self.state.lock().unwrap().entries.get_mut(&key.logical()) {
            entry.handle = Some(handle);
        }
    }
    pub fn new(home: PathBuf, instance: String) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(HashMap::new()),
            plugins: Arc::new(fae_agent::plugin::PluginManager::new(home.clone())),
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
            requests: Mutex::new(HashMap::new()),
            plugins: Arc::new(fae_agent::plugin::PluginManager::new(home.clone())),
            home,
            instance,
            state: Mutex::new(State::default()),
            timer: Some(timer),
        })
    }
    fn persist_lark_binding(
        &self,
        open: &StoredSessionSpec,
        key: &Key,
    ) -> Result<(), ProtocolError> {
        let Some(binding) = &open.binding else {
            return Ok(());
        };
        if binding.session_mapping_version != 2 {
            return Ok(());
        }
        let check = || -> anyhow::Result<()> {
            anyhow::ensure!(
                crate::lark::identity::owns_session(binding, &key.session),
                "Thread/session mismatch"
            );
            let dir = self.home.join("lark/service-bindings");
            std::fs::create_dir_all(&dir)?;
            let id = crate::lark::identity::hash(&[
                "fae-lark-service-binding-v2",
                &key.user,
                &key.session,
            ]);
            let path = dir.join(format!("{id}.json"));
            let mut proposed = open.clone();
            proposed.resume = None;
            proposed.session_id = Some(key.session.clone());
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let saved: StoredLarkBinding = serde_json::from_slice(&bytes)?;
                    anyhow::ensure!(
                        saved.open.binding == proposed.binding
                            && saved.target_id == key.target
                            && serde_json::to_value(&saved.open.target)?
                                == serde_json::to_value(&proposed.target)?,
                        "Session binding cannot change"
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            super::lifecycle::atomic_write(
                &path,
                &serde_json::to_vec(&StoredLarkBinding {
                    open: proposed,
                    target_id: key.target.clone(),
                })?,
                true,
            )?;
            Ok(())
        };
        check().map_err(|e| ProtocolError::new("session_binding_conflict", e))
    }
    pub fn counts(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (
            state.entries.len(),
            state.entries.values().filter(|e| e.active()).count(),
        )
    }
    pub fn reserve_round(&self, key: &Key) -> Result<(), ProtocolError> {
        let mut state = self.state.lock().unwrap();
        if state.entries.get(&key.logical()).is_some_and(Entry::active) {
            return Err(ProtocolError::new("session_busy", "Execution is active"));
        }
        if state.stopping || state.entries.values().filter(|e| e.active()).count() >= 8 {
            return Err(ProtocolError::new(
                "resource_exhausted",
                "At most 8 executions can run; service may be stopping",
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
            .get_mut(&key.logical())
            .ok_or_else(|| ProtocolError::new("session_not_found", "Session expired"))?;
        if entry.active() {
            return Err(ProtocolError::new("session_busy", "Execution is active"));
        }
        entry.pending_admission = true;
        entry.admission_target = Some(key.target.clone());
        Ok(())
    }
    pub fn finish_round(&self, key: &Key) -> bool {
        if let Some(entry) = self.state.lock().unwrap().entries.get_mut(&key.logical()) {
            let close = entry.timer && !entry.leased;
            entry.pending_admission = false;
            entry.admission_target = None;
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
            .get(&key.logical())
            .is_some_and(|entry| entry.stream == stream)
        {
            state.entries.remove(&key.logical());
        }
    }
    pub fn subscribed(&self, key: &Key, lease: &str) {
        if let Some(entry) = self.state.lock().unwrap().entries.get_mut(&key.logical()) {
            entry.leases.insert(lease.into());
            entry.leased = true;
            entry.disconnected = None;
        }
    }
    pub fn detach(&self, key: &Key, stream: &str, lease: &str) {
        if let Some(entry) = self
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut(&key.logical())
            .filter(|e| e.stream == stream)
        {
            entry.leases.remove(lease);
            entry.leased = !entry.leases.is_empty();
            if !entry.leased {
                entry.disconnected = Some(Instant::now());
            }
        }
    }
    pub fn lookup(
        &self,
        sid: &str,
        principal: Option<&LarkPrincipal>,
    ) -> Result<(Key, mpsc::Sender<Command>), ProtocolError> {
        let state = self.state.lock().unwrap();
        let mut matches = state.entries.values().filter(|entry| {
            entry.key.session == sid
                && !entry.closing
                && match principal {
                    Some(p) => entry.open.binding.as_ref() == Some(p),
                    None => entry.open.binding.is_none(),
                }
        });
        let entry = matches
            .next()
            .ok_or_else(|| ProtocolError::new("session_not_found", "Session does not exist"))?;
        if matches.next().is_some() {
            return Err(ProtocolError::new(
                "session_binding_conflict",
                "Ambiguous session identity",
            ));
        }
        Ok((entry.key.clone(), entry.tx.clone()))
    }
    pub async fn create(
        self: &Arc<Self>,
        request: Message,
        principal: Option<LarkPrincipal>,
        out: Outbound,
    ) -> Result<(Key, mpsc::Sender<Command>), ProtocolError> {
        let RequestPayload::SessionCreate(create) = request
            .as_request()
            .map_err(|e| ProtocolError::new("invalid_message", e))?
        else {
            return Err(ProtocolError::new(
                "invalid_message",
                "Expected session/create",
            ));
        };
        // A create never resumes or subscribes. An explicit existing SID is resolved by get.
        let builder = SingleAgentPlanBuilder::with_home_dir(&self.home);
        let (target, user, model, definition) = match &create.target {
            PlanTarget::Agent { agent_id } => {
                let source = SingleAgentSource::AgentId(agent_id.clone());
                let (config, _) = builder
                    .load_config(&source)
                    .await
                    .map_err(|e| ProtocolError::new("config_not_found", e))?;
                (
                    config.agent.name.clone(),
                    principal
                        .as_ref()
                        .map(|p| p.user_id.clone())
                        .unwrap_or(config.agent.user_id),
                    config.model.model,
                    DefinitionTarget::Agent { source },
                )
            }
            PlanTarget::Workflow { workflow_id } => (
                workflow_id.clone(),
                fae_agent::DEFAULT_USER_ID.into(),
                "workflow".into(),
                DefinitionTarget::Workflow {
                    workflow_id: workflow_id.clone(),
                    input: serde_json::Value::Null,
                },
            ),
        };
        let cache = self.requests_for(&user);
        let mut requests = cache.lock().await;
        let content = serde_json::to_string(&request).unwrap();
        let rid = request.request_id.as_ref().unwrap().to_string();
        if let Some(response) = requests.get(&rid, &content)? {
            let sid = response.session_id.as_ref().ok_or_else(|| {
                ProtocolError::new("session_not_found", "Cached session is unavailable")
            })?;
            let entry = self.lookup(sid.as_str(), principal.as_ref())?;
            out.send(fae_agent::protocol::ServerFrame::Response(response));
            return Ok(entry);
        }
        let session = principal
            .as_ref()
            .map(crate::lark::identity::session_id)
            .unwrap_or_else(wd_tools::uuid::v4);
        let key = Key {
            kind: if matches!(create.target, PlanTarget::Agent { .. }) {
                "agent"
            } else {
                "workflow"
            }
            .into(),
            target: target.clone(),
            user: user.clone(),
            session: session.clone(),
        };
        SessionRuntimeCheck::validate(&self.home, &key)?;
        let store = SessionWorkspace::new(&self.home, &key)?;
        let workspace =
            store.resolve(create.workspace.as_deref().map(std::path::Path::new), None)?;
        let open = StoredSessionSpec {
            target: definition.clone(),
            session_id: Some(session.clone()),
            workspace: Some(workspace.clone()),
            binding: principal.clone(),
            resume: None,
        };
        let stream = wd_tools::uuid::v4();
        let (tx, rx) = mpsc::channel(512);
        let (done, closed) = tokio::sync::watch::channel(None);
        {
            let mut state = self.state.lock().unwrap();
            if state.stopping || state.entries.len() >= 32 {
                return Err(ProtocolError::new(
                    "resource_exhausted",
                    "At most 32 sessions may be retained",
                ));
            }
            if state
                .entries
                .values()
                .any(|e| e.key.session == session && e.key.user == user)
            {
                return Err(ProtocolError::new(
                    "session_binding_conflict",
                    "Session already exists; use get",
                ));
            }
            self.persist_lark_binding(&open, &key)?;
            store.save(&workspace)?;
            state.entries.insert(
                key.logical(),
                Entry {
                    handle: None,
                    timer: false,
                    closed,
                    key: key.clone(),
                    open,
                    stream: stream.clone(),
                    tx: tx.clone(),
                    leased: false,
                    leases: HashSet::new(),
                    pending_admission: false,
                    admission_target: None,
                    closing: false,
                    disconnected: Some(Instant::now()),
                },
            );
        }
        let info = SessionFacts {
            target_id: target,
            name: create.name.clone(),
            user_id: user,
            model,
            session_id: session,
            workspace,
        };
        let mut actor = Actor::new(
            self.clone(),
            key.clone(),
            stream,
            definition,
            info,
            builder,
            tx.clone(),
            done,
        );
        actor.configure(create);
        if let Some(binding) = principal {
            actor.bind_lark_environment(binding);
        }
        tokio::spawn(actor.run(rx));
        let (reply, completed) = oneshot::channel();
        tx.send(Command::Create {
            request,
            out,
            reply,
        })
        .await
        .map_err(|e| ProtocolError::new("internal_error", e))?;
        let response = completed
            .await
            .map_err(|e| ProtocolError::new("internal_error", e))?;
        requests.push(rid, content, response);
        Ok((key, tx))
    }
    pub fn timer_capacity(&self) -> bool {
        let state = self.state.lock().unwrap();
        !state.stopping
            && state.entries.len() < 32
            && state.entries.values().filter(|e| e.active()).count() < 8
            && state
                .entries
                .values()
                .filter(|e| e.active() && e.timer)
                .count()
                < fae_engine::TIMER_CONCURRENT_LIMIT
    }
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
        if let SingleAgentSource::Paths { config, prompt } = &binding.source {
            if !config.is_absolute() || !prompt.is_absolute() {
                return Err(ProtocolError::new(
                    "config_not_found",
                    "Timer source paths must be absolute",
                ));
            }
        }
        let mut builder = SingleAgentPlanBuilder::with_home_dir(&self.home);
        builder.set_workspace(binding.workspace.clone());
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
        let key = Key {
            kind: "agent".into(),
            target: occurrence.owner.agent_id.clone(),
            user: occurrence.owner.user_id.clone(),
            session: occurrence.session_id.clone(),
        };
        SessionRuntimeCheck::validate(&self.home, &key)?;
        let target = DefinitionTarget::Agent {
            source: binding.source.clone(),
        };
        let open = StoredSessionSpec {
            target: target.clone(),
            workspace: Some(binding.workspace.clone()),
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
                || state.entries.values().filter(|e| e.active()).count() >= 8
                || state
                    .entries
                    .values()
                    .filter(|e| e.active() && e.timer)
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
            if state.entries.contains_key(&key.logical()) {
                return Err(ProtocolError::new(
                    "session_binding_conflict",
                    "Timer session already exists",
                ));
            }
            SessionWorkspace::new(&self.home, &key)?.save(&binding.workspace)?;
            state.entries.insert(
                key.logical(),
                Entry {
                    handle: None,
                    timer: true,
                    closed,
                    key: key.clone(),
                    open,
                    stream: stream.clone(),
                    tx: tx.clone(),
                    leased: false,
                    leases: HashSet::new(),
                    pending_admission: true,
                    admission_target: Some(key.target.clone()),
                    closing: false,
                    disconnected: None,
                },
            );
        }
        let info = SessionFacts {
            target_id: key.target.clone(),
            name: config.agent.name,
            user_id: key.user.clone(),
            model: config.model.model,
            session_id: key.session.clone(),
            workspace: binding.workspace.clone(),
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
                .any(|e| e.key.user == key.user && e.active() && e.uses_target(&key.target))
        {
            return Err(ProtocolError::new(
                "cleanup_busy",
                "Agent/user has active executions",
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
            state
                .entries
                .values_mut()
                .filter(|e| {
                    !e.leased
                        && !e.active()
                        && !e.closing
                        && e.disconnected
                            .is_some_and(|t| t.elapsed() >= Duration::from_secs(1800))
                })
                .map(|e| {
                    e.closing = true;
                    e.tx.clone()
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
        let mut completions = vec![];
        for (tx, closed) in entries {
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
struct SessionRuntimeCheck;
impl SessionRuntimeCheck {
    fn validate(home: &std::path::Path, key: &Key) -> Result<(), ProtocolError> {
        fae_engine::SessionRuntime::with_host_dir(home)
            .session_path(&key.target, &key.user, &key.session)
            .map(|_| ())
            .map_err(|e| ProtocolError::new("invalid_session_id", e))
    }
}
