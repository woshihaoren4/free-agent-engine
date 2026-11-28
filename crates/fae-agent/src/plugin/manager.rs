//! Runtime workers load current sources; installation metadata lives in plugins.config.
use super::*;
#[cfg(feature = "python")]
use anyhow::Context;
use config::PluginsConfig;
#[cfg(feature = "python")]
use std::collections::HashMap;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRecord {
    pub id: String,
    pub declaration: PluginDeclaration,
    pub source: PathBuf,
    /// Identifies this runtime instance, without fingerprinting or freezing source.
    pub revision: String,
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
    pub order: Vec<String>,
}
impl PluginSnapshot {
    /// Validate complete provider topology before publishing a worker snapshot.
    pub fn validate_providers(&self) -> anyhow::Result<()> {
        let mut plans = BTreeMap::<String, Option<String>>::new();
        let mut runtimes =
            BTreeMap::<String, (String, Vec<crate::TaskType>, Option<RuntimePublicCalls>)>::new();
        for node in self.nodes.values() {
            for member in &node.members {
                for mount in &member.declaration.mounts {
                    match mount {
                        PluginMount::Plan {
                            id, next_plan_id, ..
                        } => {
                            anyhow::ensure!(!plans.contains_key(id), "duplicate Plan ID: {id}");
                            plans.insert(id.clone(), next_plan_id.clone());
                        }
                        PluginMount::Runtime {
                            id,
                            task_types,
                            public_calls,
                            ..
                        } => {
                            let mut types = parse_task_types(task_types)?;
                            types.sort_by_key(|ty| format!("{ty:?}"));
                            if let Some((owner, previous, calls)) = runtimes.get_mut(id) {
                                anyhow::ensure!(
                                    owner == &member.id
                                        && member.declaration.registration_mode
                                            == RegistrationMode::Chain,
                                    "duplicate Runtime ID: {id}"
                                );
                                anyhow::ensure!(
                                    previous == &types,
                                    "inconsistent Runtime task_types: {id}"
                                );
                                if let Some(public) = public_calls {
                                    if let Some(previous) = calls {
                                        anyhow::ensure!(
                                            previous == public,
                                            "inconsistent Runtime public_calls: {id}"
                                        );
                                    } else {
                                        *calls = Some(public.clone());
                                    }
                                }
                            } else {
                                runtimes.insert(
                                    id.clone(),
                                    (member.id.clone(), types, public_calls.clone()),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        for id in plans.keys() {
            let mut seen = HashSet::new();
            let mut current = id.as_str();
            loop {
                anyhow::ensure!(seen.insert(current), "cyclic Plan chain: {id}");
                let next = plans
                    .get(current)
                    .ok_or_else(|| anyhow::anyhow!("missing next_plan_id: {current}"))?;
                match next {
                    Some(next) => current = next,
                    None => break,
                }
            }
        }
        Ok(())
    }
    pub fn matching(&self, point: &str, location: Option<&str>) -> Vec<Arc<PluginNode>> {
        self.order
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .filter(|node| node.matches(point, location))
            .cloned()
            .collect()
    }
}
#[derive(Debug)]
pub struct PluginExecutionBinding {
    pub snapshot: Arc<PluginSnapshot>,
    pub root: crate::WeakCtx,
    pub execution_id: crate::ExecutionId,
}

type SnapshotFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;
pub struct SnapshotPublisher(pub Arc<dyn Fn(Arc<PluginSnapshot>) -> SnapshotFuture + Send + Sync>);
impl std::fmt::Debug for SnapshotPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SnapshotPublisher")
    }
}
#[derive(Debug)]
pub struct PluginManager {
    home: PathBuf,
    current: RwLock<Arc<PluginSnapshot>>,
    reload: tokio::sync::Mutex<()>,
    native_tools: RwLock<HashSet<String>>,
    publisher: RwLock<Option<SnapshotPublisher>>,
}
impl PluginManager {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            current: RwLock::new(Arc::default()),
            reload: tokio::sync::Mutex::new(()),
            native_tools: RwLock::new(HashSet::from(["default".into()])),
            publisher: RwLock::new(None),
        }
    }
    pub fn snapshot(&self) -> Arc<PluginSnapshot> {
        self.current.read().unwrap().clone()
    }
    pub fn reserve_tool(&self, channel: &str) {
        self.native_tools.write().unwrap().insert(channel.into());
    }
    /// The host builds and publishes a complete immutable Engine/adapter pair.
    /// Failure leaves the current snapshot untouched.
    pub fn set_snapshot_publisher(
        &self,
        publisher: impl Fn(Arc<PluginSnapshot>) -> SnapshotFuture + Send + Sync + 'static,
    ) {
        *self.publisher.write().unwrap() = Some(SnapshotPublisher(Arc::new(publisher)));
    }
    async fn prepare_publication(&self, snapshot: &Arc<PluginSnapshot>) -> anyhow::Result<()> {
        snapshot.validate_providers()?;
        let publisher = self.publisher.read().unwrap().as_ref().map(|p| p.0.clone());
        if let Some(publisher) = publisher {
            publisher(snapshot.clone()).await?;
        }
        Ok(())
    }

    /// Offline inspection never imports a module or calls plugin_config.
    pub fn show(home: &Path, id: &str) -> anyhow::Result<serde_json::Value> {
        if id != "*" {
            validate_id(id)?;
        }
        let config = PluginsConfig::read(home)?;
        let mut result = serde_json::Map::new();
        for (order, plugin) in config.plugins.iter().enumerate() {
            if id != "*" && id != plugin.id {
                continue;
            }
            let source = plugin.source_path(home);
            let declaration = if id != "*" && plugin.installed && source.is_dir() {
                Some(
                    serde_json::from_slice::<PluginDeclaration>(&std::fs::read(
                        source.join("plugin.config"),
                    )?)?
                    .normalize(&plugin.id, None)?,
                )
            } else {
                None
            };
            result.insert(
                plugin.id.clone(),
                serde_json::json!({
                    "id":plugin.id,"order":order + 1,"installed":plugin.installed,
                    "enabled":plugin.enabled,"source":source,"declaration":declaration,
                    "loaded":false,"host":"offline","runtime":null
                }),
            );
        }
        anyhow::ensure!(id == "*" || !result.is_empty(), "unknown plugin: {id}");
        Ok(result.into())
    }
    pub fn status(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        let mut value = Self::show(&self.home, id)?;
        for record in value.as_object_mut().unwrap().values_mut() {
            record["host"] = "online".into();
        }
        for node in self.snapshot().nodes.values() {
            for member in &node.members {
                if let Some(item) = value.get_mut(&member.id) {
                    item["loaded"] = true.into();
                    item["declaration"] = serde_json::to_value(&member.declaration)?;
                    item["entry_id"] = node.id.clone().into();
                    item["load_id"] = node.load_id.clone().into();
                    item["revision"] = member.revision.clone().into();
                    item["chain_order"] =
                        if member.declaration.registration_mode == RegistrationMode::Chain {
                            serde_json::json!(
                                node.members
                                    .iter()
                                    .map(|member| &member.id)
                                    .collect::<Vec<_>>()
                            )
                        } else {
                            serde_json::Value::Null
                        };
                    item["instances"] = serde_json::json!(member.declaration.mounts.iter().enumerate()
                        .map(|(index,mount)|serde_json::json!({"instance_id":format!("{}:{}:{index}",node.load_id,member.id),"mount":mount}))
                        .collect::<Vec<_>>());
                    item["runtime"] = node.executor.status();
                }
            }
        }
        Ok(value)
    }

    /// Load only installed, enabled plugins, in the configured installation order.
    /// Any configuration, import, worker or mount error fails startup.
    pub async fn load_all(&self) -> anyhow::Result<()> {
        let config = PluginsConfig::read(&self.home)?;
        if !self.home.join("plugins/plugins.config").exists() {
            config.write(&self.home)?;
        }
        let enabled = config
            .plugins
            .iter()
            .filter(|plugin| plugin.installed && plugin.enabled)
            .collect::<Vec<_>>();
        #[cfg(not(feature = "python"))]
        anyhow::ensure!(
            enabled.is_empty(),
            "plugin_engine_unavailable: Python support was not compiled"
        );
        #[cfg(feature = "python")]
        {
            let _guard = self.reload.lock().await;
            let mut records = Vec::new();
            for plugin in enabled {
                let source = plugin
                    .source_path(&self.home)
                    .canonicalize()
                    .with_context(|| format!("plugin source {}", plugin.id))?;
                anyhow::ensure!(
                    plugin.directory_path.is_empty() || source.is_dir(),
                    "plugin directory_path is not a directory: {}",
                    plugin.id
                );
                let declaration = crate::hook::hook_py::executor::inspect(&plugin.id, &source)
                    .await
                    .with_context(|| format!("inspect plugin {}", plugin.id))?;
                records.push(PluginRecord {
                    id: plugin.id.clone(),
                    declaration,
                    source,
                    revision: wd_tools::uuid::v4(),
                });
            }
            let chain = records
                .iter()
                .filter(|record| record.declaration.registration_mode == RegistrationMode::Chain)
                .cloned()
                .collect::<Vec<_>>();
            let mut snapshot = PluginSnapshot::default();
            for record in records {
                let members = if record.declaration.registration_mode == RegistrationMode::Chain {
                    if record.id != chain[0].id {
                        continue;
                    }
                    chain.clone()
                } else {
                    vec![record]
                };
                let id = members[0].id.clone();
                let worker = crate::hook::hook_py::executor::PythonExecutor::start(members.clone())
                    .await
                    .with_context(|| {
                        format!(
                            "start plugin group {}",
                            members
                                .iter()
                                .map(|member| member.id.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?;
                snapshot.order.push(id.clone());
                snapshot.nodes.insert(
                    id.clone(),
                    Arc::new(PluginNode {
                        id,
                        load_id: wd_tools::uuid::v4(),
                        members,
                        executor: Arc::new(worker),
                    }),
                );
            }
            self.check_conflicts(&snapshot)?;
            let snapshot = Arc::new(snapshot);
            self.prepare_publication(&snapshot).await?;
            *self.current.write().unwrap() = snapshot;
        }
        Ok(())
    }

    pub async fn load(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        let _guard = self.reload.lock().await;
        self.load_locked(id).await
    }

    /// Copy and activate a plugin without restarting the host.
    pub async fn install(&self, source: &Path) -> anyhow::Result<String> {
        let _guard = self.reload.lock().await;
        let previous_config = PluginsConfig::read(&self.home)?;
        let id = config::install(&self.home, source)?;
        if let Err(error) = self.load_locked(&id).await {
            previous_config.write(&self.home)?;
            return Err(error);
        }
        Ok(id)
    }

    /// Remove runtime registrations while retaining the installed source.
    pub async fn uninstall(&self, id: &str) -> anyhow::Result<()> {
        validate_id(id)?;
        let _guard = self.reload.lock().await;
        let config = PluginsConfig::read(&self.home)?;
        anyhow::ensure!(
            config
                .plugins
                .iter()
                .any(|plugin| plugin.id == id && plugin.installed),
            "plugin is not installed: {id}"
        );
        let previous = self.snapshot();
        let mut nodes = previous.nodes.clone();
        if let Some(node) = previous
            .nodes
            .values()
            .find(|node| node.members.iter().any(|member| member.id == id))
        {
            nodes.remove(&node.id);
            #[cfg(feature = "python")]
            if node.members.len() > 1 {
                let members = node
                    .members
                    .iter()
                    .filter(|member| member.id != id)
                    .cloned()
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
        }
        let mut order = config
            .plugins
            .iter()
            .filter(|plugin| nodes.contains_key(&plugin.id))
            .map(|plugin| plugin.id.clone())
            .collect::<Vec<_>>();
        for entry in &previous.order {
            if nodes.contains_key(entry) && !order.contains(entry) {
                order.push(entry.clone());
            }
        }
        let snapshot = Arc::new(PluginSnapshot { nodes, order });
        config::uninstall(&self.home, id)?;
        if let Err(error) = self.prepare_publication(&snapshot).await {
            config.write(&self.home)?;
            return Err(error);
        }
        *self.current.write().unwrap() = snapshot;
        Ok(())
    }

    async fn load_locked(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        validate_id(id)?;
        #[cfg(not(feature = "python"))]
        anyhow::bail!("plugin_engine_unavailable: Python support was not compiled");
        #[cfg(feature = "python")]
        {
            let source = locate(&self.home, id)?.canonicalize()?;
            let declaration = crate::hook::hook_py::executor::inspect(id, &source).await?;
            let record = PluginRecord {
                id: id.into(),
                declaration,
                source,
                revision: wd_tools::uuid::v4(),
            };
            let previous = self.snapshot();
            let mut records = previous
                .nodes
                .values()
                .flat_map(|node| node.members.iter())
                .map(|record| (record.id.clone(), record.clone()))
                .collect::<BTreeMap<_, _>>();
            records.insert(id.into(), record.clone());
            let mut installation_order = previous
                .order
                .iter()
                .flat_map(|id| {
                    previous.nodes[id]
                        .members
                        .iter()
                        .map(|member| member.id.clone())
                })
                .collect::<Vec<_>>();
            if !installation_order.iter().any(|entry| entry == id) {
                installation_order.push(id.into());
            }
            // Configuration order also governs direct nodes and mixed direct/chain plugins.
            let config = PluginsConfig::read(&self.home)?;
            let mut order = config
                .plugins
                .iter()
                .filter(|plugin| records.contains_key(&plugin.id))
                .map(|plugin| plugin.id.clone())
                .collect::<Vec<_>>();
            for entry in installation_order {
                if !order.contains(&entry) {
                    order.push(entry);
                }
            }
            let chain_order = order
                .iter()
                .filter(|id| records[*id].declaration.registration_mode == RegistrationMode::Chain)
                .cloned()
                .collect::<Vec<_>>();
            let rebuild_chain = record.declaration.registration_mode == RegistrationMode::Chain
                || previous.nodes.values().any(|node| {
                    node.members.iter().any(|member| {
                        member.id == id
                            && member.declaration.registration_mode == RegistrationMode::Chain
                    })
                });
            let mut nodes = previous.nodes.clone();
            nodes.retain(|_, node| {
                !node.members.iter().any(|member| member.id == id)
                    && !(rebuild_chain
                        && node.members.iter().any(|member| {
                            member.declaration.registration_mode == RegistrationMode::Chain
                        }))
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
            if rebuild_chain && !chain_order.is_empty() {
                let members = chain_order
                    .iter()
                    .map(|id| records[id].clone())
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
            order.retain(|id| nodes.contains_key(id));
            let snapshot = Arc::new(PluginSnapshot { nodes, order });
            self.check_conflicts(&snapshot)?;
            self.prepare_publication(&snapshot).await?;
            *self.current.write().unwrap() = snapshot;
            // Embedded callers may load a source without installing it.
            if config.plugins.iter().any(|plugin| plugin.id == id) {
                self.status(id)
            } else {
                Ok(serde_json::json!({"id":id,"loaded":true}))
            }
        }
    }
    #[cfg(feature = "python")]
    fn check_conflicts(&self, snapshot: &PluginSnapshot) -> anyhow::Result<()> {
        snapshot.validate_providers()?;
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
}

#[cfg(feature = "python")]
fn locate(home: &Path, id: &str) -> anyhow::Result<PathBuf> {
    let root = home.join("plugins");
    let config = PluginsConfig::read(home)?;
    if let Some(plugin) = config.plugins.iter().find(|plugin| plugin.id == id) {
        anyhow::ensure!(
            plugin.installed && plugin.enabled,
            "plugin is not enabled: {id}"
        );
        let source = plugin.source_path(home);
        anyhow::ensure!(
            plugin.directory_path.is_empty() || source.is_dir(),
            "plugin directory_path is not a directory: {id}"
        );
        return Ok(source);
    }
    let candidates = [
        root.join(format!("{id}.py")),
        root.join(id),
        root.join(format!("{id}.js")),
    ];
    let existing = candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| anyhow::anyhow!("plugin_source_invalid: missing plugin {id}"))?;
    Ok(existing)
}
