//! Candidate workers are prepared before publication. Runs retain immutable snapshots.
use super::*;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, RwLock, Weak},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRecord {
    pub id: String,
    pub declaration: PluginDeclaration,
    pub source: PathBuf,
    pub revision: String,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct PluginNode {
    pub id: String,
    pub load_id: String,
    pub members: Vec<PluginRecord>,
    pub executor: Arc<dyn PluginExecutor>,
}
impl PluginNode {
    pub fn matches(&self, point: &str, location: Option<&str>) -> bool {
        self.members.iter().any(|r| {
            r.declaration.mounts.iter().any(|m| {
                m.point() == point
                    && (m.location() == location
                        || (point == "SingleAgentHook" && m.location().is_none())
                        || point == "ModelHook")
            })
        })
    }
}
#[derive(Debug, Default)]
pub struct PluginSnapshot {
    pub nodes: BTreeMap<String, Arc<PluginNode>>,
}
impl PluginSnapshot {
    pub fn matching(&self, point: &str, location: Option<&str>) -> Vec<Arc<PluginNode>> {
        self.nodes
            .values()
            .filter(|n| n.matches(point, location))
            .cloned()
            .collect()
    }
}
#[derive(Debug)]
pub struct PluginRun {
    pub snapshot: Arc<PluginSnapshot>,
    pub root: crate::WeakCtx,
    pub run_id: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    records: BTreeMap<String, PluginRecord>,
    chain_order: Vec<String>,
}
// Existing activated snapshots may still contain the old static model mount.
// Migrate metadata only; frozen plugin code must still implement match_model.
fn read_saved(bytes: &[u8]) -> anyhow::Result<Saved> {
    let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
    if let Some(records) = value
        .get_mut("records")
        .and_then(serde_json::Value::as_object_mut)
    {
        for record in records.values_mut() {
            if let Some(mounts) = record
                .get_mut("declaration")
                .and_then(|declaration| declaration.get_mut("mounts"))
                .and_then(serde_json::Value::as_array_mut)
            {
                for mount in mounts {
                    if mount["point"] == "ModelHook" {
                        if let Some(mount) = mount.as_object_mut() {
                            mount.remove("model");
                        }
                    }
                }
            }
        }
    }
    Ok(serde_json::from_value(value)?)
}
#[derive(Debug)]
pub struct PluginManager {
    home: PathBuf,
    current: RwLock<Arc<PluginSnapshot>>,
    saved: RwLock<Saved>,
    reload: tokio::sync::Mutex<()>,
    native_tools: RwLock<HashSet<String>>,
    diagnostics: RwLock<HashMap<String, String>>,
    retired: RwLock<Vec<Weak<PluginNode>>>,
}
impl PluginManager {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            current: RwLock::new(Arc::default()),
            saved: RwLock::new(Saved::default()),
            reload: tokio::sync::Mutex::new(()),
            native_tools: RwLock::new(HashSet::from(["default".into()])),
            diagnostics: RwLock::new(HashMap::new()),
            retired: RwLock::new(Vec::new()),
        }
    }
    pub fn snapshot(&self) -> Arc<PluginSnapshot> {
        self.current.read().unwrap().clone()
    }
    pub fn reserve_tool(&self, channel: &str) {
        self.native_tools.write().unwrap().insert(channel.into());
    }
    fn state_path(&self) -> PathBuf {
        self.home.join("plugin-state/active.json")
    }

    /// Offline inspection never imports a module or calls plugin_config.
    pub fn show(home: &Path, id: &str) -> anyhow::Result<serde_json::Value> {
        if id != "*" {
            validate_id(id)?;
        }
        let saved = match std::fs::read(home.join("plugin-state/active.json")) {
            Ok(bytes) => read_saved(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Saved::default(),
            Err(e) => return Err(e.into()),
        };
        let mut result = BTreeMap::<String, serde_json::Value>::new();
        for (key, r) in &saved.records {
            if id == "*" || id == key {
                result.insert(key.clone(), serde_json::json!({"declaration":r.declaration,
                    "revision":r.revision,"source":locate(&home.join("plugins"),key).ok(),"frozen_source":r.source,
                    "loaded":false,"host":"offline","runtime":null,"error":r.error,
                    "entry_id": if r.declaration.registration_mode == RegistrationMode::Chain { saved.chain_order.first() } else { Some(key) },
                    "chain_order": if r.declaration.registration_mode == RegistrationMode::Chain { Some(&saved.chain_order) } else { None },
                    "role":if r.declaration.registration_mode == RegistrationMode::Chain && saved.chain_order.first() != Some(key) {"python_chain_member"} else {"rust_entry"},
                    "source_changed": source_fingerprint(&home.join("plugins"), key).ok().as_deref()!=Some(&r.revision)}));
            }
        }
        if let Ok(entries) = std::fs::read_dir(home.join("plugins")) {
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                let key = if path.is_dir() {
                    path.file_name()
                } else {
                    path.file_stem()
                };
                let Some(key) = key.and_then(|s| s.to_str()) else {
                    continue;
                };
                if validate_id(key).is_err() || (id != "*" && id != key) || result.contains_key(key)
                {
                    continue;
                }
                let declaration = if path.is_dir() {
                    std::fs::read(path.join("plugin.config"))
                        .ok()
                        .and_then(|b| serde_json::from_slice::<PluginDeclaration>(&b).ok())
                        .and_then(|d| d.normalize(key, None).ok())
                } else {
                    None
                };
                result.insert(key.into(), serde_json::json!({"declaration":declaration,"source":path,"loaded":false,"host":"offline","runtime":null,"status":"not_loaded"}));
            }
        }
        Ok(serde_json::to_value(result)?)
    }
    pub fn status(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        let mut value = Self::show(&self.home, id)?;
        if let Some(records) = value.as_object_mut() {
            for record in records.values_mut() {
                record["host"] = "online".into();
                record["retired_versions"] = self
                    .retired
                    .read()
                    .unwrap()
                    .iter()
                    .filter(|n| n.strong_count() > 0)
                    .count()
                    .into();
            }
        }
        for (id, error) in self.diagnostics.read().unwrap().iter() {
            if let Some(record) = value.get_mut(id) {
                record["error"] = error.clone().into();
            }
        }
        for node in self.snapshot().nodes.values() {
            for member in &node.members {
                if let Some(item) = value.get_mut(&member.id) {
                    item["loaded"] = true.into();
                    item["entry_id"] = node.id.clone().into();
                    item["load_id"] = node.load_id.clone().into();
                    item["instances"] = serde_json::json!(member.declaration.mounts.iter().enumerate()
                        .map(|(index,mount)|serde_json::json!({"instance_id":format!("{}:{}:{index}",node.load_id,member.id),"mount":mount}))
                        .collect::<Vec<_>>());
                    item["runtime"] = node.executor.status();
                }
            }
        }
        Ok(value)
    }

    pub async fn restore(&self) -> anyhow::Result<()> {
        let bytes = match tokio::fs::read(self.state_path()).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let saved = read_saved(&bytes)?;
        // Restore frozen declarations and sources; never run plugin_config on restart.
        {
            let mut state = self.saved.write().unwrap();
            state.chain_order = saved.chain_order.clone();
            state.records = saved.records.clone();
        }
        #[cfg(feature = "python")]
        {
            let _guard = self.reload.lock().await;
            let mut groups = saved
                .records
                .values()
                .filter(|r| r.declaration.registration_mode == RegistrationMode::Direct)
                .map(|r| vec![r.clone()])
                .collect::<Vec<_>>();
            let chain = saved
                .chain_order
                .iter()
                .map(|id| {
                    saved
                        .records
                        .get(id)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("missing persisted chain member {id}"))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if !chain.is_empty() {
                groups.push(chain);
            }
            let mut nodes = BTreeMap::new();
            for members in groups {
                let ids = members.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
                let result =
                    crate::hook::hook_py::executor::PythonExecutor::start(members.clone()).await;
                match result {
                    Ok(worker) => {
                        let root = members[0].id.clone();
                        let node = Arc::new(PluginNode {
                            id: root.clone(),
                            load_id: wd_tools::uuid::v4(),
                            members,
                            executor: Arc::new(worker),
                        });
                        nodes.insert(root.clone(), node);
                        if let Err(error) = self.check_conflicts(&PluginSnapshot {
                            nodes: nodes.clone(),
                        }) {
                            nodes.remove(&root);
                            for id in ids {
                                self.diagnostics
                                    .write()
                                    .unwrap()
                                    .insert(id, error.to_string());
                            }
                        }
                    }
                    Err(error) => {
                        for id in ids {
                            self.diagnostics
                                .write()
                                .unwrap()
                                .insert(id, error.to_string());
                        }
                    }
                }
            }
            *self.current.write().unwrap() = Arc::new(PluginSnapshot { nodes });
        }
        Ok(())
    }

    pub async fn load(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        validate_id(id)?;
        let _guard = self.reload.lock().await;
        let result = self.load_candidate(id).await;
        if let Err(error) = &result {
            self.diagnostics
                .write()
                .unwrap()
                .insert(id.into(), error.to_string());
        } else {
            self.diagnostics.write().unwrap().remove(id);
        }
        result
    }
    async fn load_candidate(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        #[cfg(not(feature = "python"))]
        anyhow::bail!("plugin_engine_unavailable: Python support was not compiled");
        #[cfg(feature = "python")]
        {
            let record = self.prepare(id).await?;
            {
                let mut retired = self.retired.write().unwrap();
                retired.retain(|node| node.strong_count() > 0);
                anyhow::ensure!(retired.len() < 16, "plugin_busy: retained_versions");
            }
            let (mut records, mut order) = {
                let saved = self.saved.read().unwrap();
                (saved.records.clone(), saved.chain_order.clone())
            };
            records.insert(id.into(), record.clone());
            if record.declaration.registration_mode == RegistrationMode::Chain
                && !order.iter().any(|x| x == id)
            {
                order.push(id.into());
            }
            order.retain(|id| {
                records
                    .get(id)
                    .is_some_and(|r| r.declaration.registration_mode == RegistrationMode::Chain)
            });
            let previous = self.snapshot();
            let mut nodes = previous.nodes.clone();
            let rebuild_chain = record.declaration.registration_mode == RegistrationMode::Chain
                || previous.nodes.values().any(|n| {
                    n.members.iter().any(|r| {
                        r.id == id && r.declaration.registration_mode == RegistrationMode::Chain
                    })
                });
            // Replace the old direct entry or whole old chain as one atomic publication.
            nodes.retain(|_, n| {
                !n.members.iter().any(|r| r.id == id)
                    && !(rebuild_chain
                        && n.members
                            .iter()
                            .any(|r| r.declaration.registration_mode == RegistrationMode::Chain))
            });
            if record.declaration.registration_mode == RegistrationMode::Direct {
                let worker =
                    crate::hook::hook_py::executor::PythonExecutor::start(vec![record.clone()])
                        .await?;
                nodes.insert(
                    id.into(),
                    Arc::new(PluginNode {
                        id: id.into(),
                        load_id: wd_tools::uuid::v4(),
                        members: vec![record],
                        executor: Arc::new(worker),
                    }),
                );
            }
            if rebuild_chain && !order.is_empty() {
                let members = order
                    .iter()
                    .filter_map(|id| records.get(id).cloned())
                    .collect::<Vec<_>>();
                let root = members[0].id.clone();
                let worker =
                    crate::hook::hook_py::executor::PythonExecutor::start(members.clone()).await?;
                nodes.insert(
                    root.clone(),
                    Arc::new(PluginNode {
                        id: root,
                        load_id: wd_tools::uuid::v4(),
                        members,
                        executor: Arc::new(worker),
                    }),
                );
            }
            let snapshot = Arc::new(PluginSnapshot { nodes });
            self.check_conflicts(&snapshot)?;
            anyhow::ensure!(
                source_fingerprint(&self.home.join("plugins"), id)? == records[id].revision,
                "plugin_source_changed_during_load"
            );
            let saved = Saved {
                records,
                chain_order: order,
            };
            let path = self.state_path();
            let tmp = path.with_extension(format!("{}.tmp", wd_tools::uuid::v4()));
            tokio::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?).await?;
            tokio::fs::rename(&tmp, &path).await?;
            *self.saved.write().unwrap() = saved;
            self.retired.write().unwrap().extend(
                previous
                    .nodes
                    .values()
                    .filter(|old| !snapshot.nodes.values().any(|new| Arc::ptr_eq(old, new)))
                    .map(Arc::downgrade),
            );
            *self.current.write().unwrap() = snapshot;
            self.status(id)
        }
    }
    fn check_conflicts(&self, snapshot: &PluginSnapshot) -> anyhow::Result<()> {
        let mut routes = HashMap::<(String, String), String>::new();
        for node in snapshot.nodes.values() {
            for r in &node.members {
                for m in &r.declaration.mounts {
                    if m.point() != "ToolsHook" {
                        continue;
                    }
                    let location = m.location().unwrap();
                    anyhow::ensure!(
                        !self.native_tools.read().unwrap().contains(location),
                        "plugin_mount_conflict: native {location}"
                    );
                    let key = (m.point().into(), location.into());
                    if let Some(owner) = routes.insert(key, node.id.clone()) {
                        anyhow::ensure!(
                            owner == node.id
                                && r.declaration.registration_mode == RegistrationMode::Chain,
                            "plugin_mount_conflict: {location}"
                        );
                    }
                }
            }
        }
        Ok(())
    }
    #[cfg(feature = "python")]
    async fn prepare(&self, id: &str) -> anyhow::Result<PluginRecord> {
        let plugins = self.home.join("plugins");
        tokio::fs::create_dir_all(&plugins).await?;
        let source = locate(&plugins, id)?;
        let revision = source_fingerprint(&plugins, id)?;
        let cache = self
            .home
            .join("plugin-state/versions")
            .join(id)
            .join(&revision);
        if !cache.exists() {
            let temp = cache.with_extension(format!("{}.tmp", wd_tools::uuid::v4()));
            tokio::fs::create_dir_all(&temp).await?;
            copy_source(&source, &temp)?;
            tokio::fs::rename(&temp, &cache).await?;
        }
        let source = if source.is_file() {
            cache.join(source.file_name().unwrap())
        } else {
            cache
        };
        let declaration = crate::hook::hook_py::executor::inspect(id, &source).await?;
        anyhow::ensure!(
            source_fingerprint(&plugins, id)? == revision,
            "plugin_source_changed_during_load"
        );
        Ok(PluginRecord {
            id: id.into(),
            declaration,
            source,
            revision,
            error: None,
        })
    }
}
fn locate(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    let candidates = [
        root.join(id),
        root.join(format!("{id}.py")),
        root.join(format!("{id}.js")),
    ];
    let existing: Vec<_> = candidates.into_iter().filter(|p| p.exists()).collect();
    anyhow::ensure!(
        existing.len() == 1,
        "plugin_source_invalid: missing or ambiguous plugin {id}"
    );
    anyhow::ensure!(
        existing[0]
            .canonicalize()?
            .starts_with(root.canonicalize()?),
        "plugin_source_invalid: source escapes plugin root"
    );
    anyhow::ensure!(
        existing[0].extension().and_then(|s| s.to_str()) != Some("js"),
        "plugin_engine_unavailable: js"
    );
    Ok(existing[0].clone())
}
fn files(root: &Path, current: &Path, result: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') || name == "__pycache__" {
            continue;
        }
        anyhow::ensure!(
            !entry.file_type()?.is_symlink(),
            "plugin source symlinks are not supported"
        );
        if path.is_dir() {
            files(root, &path, result)?;
        } else {
            result.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    result.sort();
    Ok(())
}
fn source_fingerprint(root: &Path, id: &str) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let source = locate(root, id)?;
    let mut hash = Sha256::new();
    let mut entries = Vec::new();
    if source.is_file() {
        hash.update(std::fs::read(&source)?);
    } else {
        files(&source, &source, &mut entries)?;
        for file in entries {
            hash.update(file.to_string_lossy().as_bytes());
            hash.update([0]);
            hash.update(std::fs::read(source.join(file))?);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn copy_source(source: &Path, target: &Path) -> anyhow::Result<()> {
    if source.is_file() {
        std::fs::copy(source, target.join(source.file_name().unwrap()))?;
    } else {
        let mut entries = Vec::new();
        files(source, source, &mut entries)?;
        for file in entries {
            let to = target.join(&file);
            std::fs::create_dir_all(to.parent().unwrap())?;
            std::fs::copy(source.join(file), to)?;
        }
    }
    Ok(())
}
