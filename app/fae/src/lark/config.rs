use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u16,
    #[serde(alias = "app_key")]
    pub app_id: String,
    pub app_secret: String,
    pub tenant_key: String,
    pub agent_id: String,
    #[serde(default = "default_workspace")]
    pub workspace: PathBuf,
    #[serde(default)]
    pub group: Group,
    #[serde(default)]
    pub execution: Execution,
    #[serde(default)]
    pub output: Output,
    #[serde(default)]
    pub storage: Storage,
}

pub fn default_workspace() -> PathBuf {
    PathBuf::from("~")
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Group {
    pub require_mention: bool,
    pub reply_in_thread: bool,
}
impl Default for Group {
    fn default() -> Self {
        Self {
            require_mention: true,
            reply_in_thread: false,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Execution {
    pub max_concurrent_rounds: usize,
    // Accept old config files without publishing a queue setting.
    #[serde(rename = "session_queue_capacity", skip_serializing)]
    pub legacy_queue_capacity: Option<usize>,
    pub session_idle_secs: u64,
}
impl Default for Execution {
    fn default() -> Self {
        Self {
            max_concurrent_rounds: 4,
            legacy_queue_capacity: None,
            session_idle_secs: 1800,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Output {
    pub flush_interval_ms: u64,
}
impl Default for Output {
    fn default() -> Self {
        Self {
            flush_interval_ms: 300,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Storage {
    pub completed_record_retention_days: u64,
}
impl Default for Storage {
    fn default() -> Self {
        Self {
            completed_record_retention_days: 7,
        }
    }
}
impl Config {
    pub fn load(home: &Path) -> anyhow::Result<Self> {
        let mut config: Self = serde_json::from_slice(&std::fs::read(home.join("lark.config"))?)?;
        anyhow::ensure!(config.version == 1, "Unsupported lark.config version");
        for (name, value) in [
            ("app_id", &config.app_id),
            ("app_secret", &config.app_secret),
            ("tenant_key", &config.tenant_key),
            ("agent_id", &config.agent_id),
        ] {
            anyhow::ensure!(
                !value.trim().is_empty()
                    && !value.contains(['\n', '\r'])
                    && !value.starts_with('<'),
                "Invalid {name}"
            );
        }
        anyhow::ensure!(
            config.group.require_mention,
            "Group messages must require @bot"
        );
        anyhow::ensure!(
            (1..=8).contains(&config.execution.max_concurrent_rounds),
            "max_concurrent_rounds must be 1..8"
        );
        anyhow::ensure!(
            (1..=86400).contains(&config.execution.session_idle_secs),
            "session_idle_secs must be 1..86400"
        );
        anyhow::ensure!(
            (100..=5000).contains(&config.output.flush_interval_ms),
            "flush_interval_ms must be 100..5000"
        );
        anyhow::ensure!(
            (1..=365).contains(&config.storage.completed_record_retention_days),
            "retention must be 1..365 days"
        );
        config.workspace = crate::app::paths::expand_home(config.workspace);
        anyhow::ensure!(
            config.workspace.is_absolute(),
            "Lark workspace must be an absolute directory"
        );
        config.workspace = crate::app::paths::workspace(config.workspace)?;
        Ok(config)
    }
    pub async fn check_agent(&self, home: &Path) -> anyhow::Result<()> {
        use fae_agent::{SingleAgentSource, WorkflowAction, WorkflowMetadata, WorkflowNode};
        enum Dependency {
            Agent(SingleAgentSource),
            Workflow(String),
        }
        let builder = fae_agent::SingleAgentPlanBuilder::with_home_dir(home);
        let mut pending = vec![Dependency::Agent(SingleAgentSource::AgentId(
            self.agent_id.clone(),
        ))];
        let mut agents = std::collections::HashSet::new();
        let mut workflows = std::collections::HashSet::new();
        while let Some(dependency) = pending.pop() {
            match dependency {
                Dependency::Agent(source) => {
                    if !agents.insert(serde_json::to_string(&source)?) {
                        continue;
                    }
                    let (config, _) = builder.load_config(&source).await?;
                    pending.extend(
                        config
                            .sub_agents
                            .into_iter()
                            .map(|id| Dependency::Agent(SingleAgentSource::AgentId(id))),
                    );
                    pending.extend(config.workflows.into_iter().map(Dependency::Workflow));
                }
                Dependency::Workflow(id) => {
                    let mut components = Path::new(&id).components();
                    anyhow::ensure!(
                        !id.is_empty()
                            && matches!(components.next(), Some(std::path::Component::Normal(_)))
                            && components.next().is_none(),
                        "Lark workflow reference `{id}` must be a static workflow ID"
                    );
                    if !workflows.insert(id.clone()) {
                        continue;
                    }
                    let metadata = WorkflowMetadata::load_json(
                        home.join("workflows").join(format!("{id}.json")),
                    )
                    .await?;
                    for node in metadata.nodes.into_values() {
                        let WorkflowNode::Execute { action, .. } = node else {
                            continue;
                        };
                        match action {
                            WorkflowAction::SingleAgent { source, .. } => {
                                pending.push(Dependency::Agent(source));
                            }
                            WorkflowAction::Workflow { workflow_id, .. } => {
                                pending.push(Dependency::Workflow(workflow_id));
                            }
                            // Todo isolation is enforced by runtime Context plan
                            // ownership, including dynamically selected routes.
                            WorkflowAction::Tool { .. }
                            | WorkflowAction::Session { .. }
                            | WorkflowAction::Custom { .. }
                            | WorkflowAction::Python { .. } => {}
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub fn redacted(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("config serialization");
        value["app_secret"] = "***".into();
        value
    }
    pub fn init(home: &Path, workspace: PathBuf, force: bool) -> anyhow::Result<()> {
        let path = home.join("lark.config");
        anyhow::ensure!(
            force || !path.exists(),
            "lark.config already exists; use --force to replace"
        );
        let config = Self {
            version: 1,
            app_id: "<APP_ID>".into(),
            app_secret: "<APP_SECRET>".into(),
            tenant_key: "<TENANT_KEY>".into(),
            agent_id: "fae".into(),
            workspace,
            group: Group::default(),
            execution: Execution::default(),
            output: Output::default(),
            storage: Storage::default(),
        };
        crate::server::lifecycle::atomic_write(&path, &serde_json::to_vec_pretty(&config)?, true)
    }
}
