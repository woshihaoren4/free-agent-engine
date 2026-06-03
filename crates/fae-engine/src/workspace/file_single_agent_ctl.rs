use crate::{AgentCtl, RecallAgentRef, ErasedAgentConfig};
use fae_agent::{AgentConfig, AgentConfigFile, AgentEventHandleImpl, Error, FileChatMemory, FileSessionCtl, FAE_WORKSPACE};
use fae_agent::{AgentRef, Record, SingleAgent, SingleSessionMD};
use std::path::PathBuf;
use std::sync::Arc;

use std::collections::HashMap;
use serde_json::Value;
use tokio::sync::RwLock;

/// A loader that loads agents from a workspace directory structure.
pub struct SingleAgentCtlFromFile {
    workspace_dir: PathBuf,
    agents: RwLock<HashMap<String, AgentRef>>,
}

impl SingleAgentCtlFromFile {
    pub fn get_id() -> &'static str {
        "default_single_agent"
    }
    pub fn new<P: Into<PathBuf>>(workspace_dir: P) -> Self {
        Self {
            workspace_dir: workspace_dir.into(),
            agents: RwLock::new(HashMap::new()),
        }
    }
}
impl Default for SingleAgentCtlFromFile {
    fn default() -> Self {
        let base_dir = std::env::var(FAE_WORKSPACE)
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Self::new(base_dir)
    }
}

#[async_trait::async_trait]
impl AgentCtl for SingleAgentCtlFromFile {
    fn id(&self) -> &str {
        Self::get_id()
    }

    async fn load(&self, agent_id: &str) -> anyhow::Result<AgentRef> {
        {
            let cache = self.agents.read().await;
            if let Some(agent) = cache.get(agent_id) {
                return Ok(agent.clone());
            }
        }
        let agent_dir = self.workspace_dir.join(agent_id);
        if !agent_dir.exists() {
            return Err(anyhow::anyhow!(
                "[SingleAgentCtlFromFile::{}] load agent directory not found: {:?}",
                agent_id,
                agent_dir
            ));
        }

        // 1. memory
        let memory = FileChatMemory::<Record>::new(&agent_dir).await?;

        // 2. Session config
        let session_config = FileSessionCtl::<SingleSessionMD>::new(
            &agent_dir,
            |meta| meta.id.clone(),
            |_| 0, // SingleAgentSessionConfig doesn't have updated_at, return 0
        )
        .await?;

        // 3. Agent config
        let agent_config = AgentConfigFile::load(&agent_dir).await?;

        // 4. Create the agent
        let single_agent = SingleAgent::<SingleSessionMD,Record>::new(
            agent_id,
            Arc::new(memory),
            Arc::new(session_config),
            Arc::new(agent_config),
        );
        let agent = AgentEventHandleImpl::new(Arc::new(single_agent));
        let agent_ref = AgentRef::from(agent);

        let mut cache = self.agents.write().await;
        cache.insert(agent_id.to_string(), agent_ref.clone());

        Ok(agent_ref)
    }

    async fn recall(&self, _task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>> {
        Err(Error::NoSupport.into())
    }

    async fn list(&self, limit: usize, offset: usize) -> anyhow::Result<Vec<AgentRef>> {
        if !self.workspace_dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = tokio::fs::read_dir(&self.workspace_dir).await?;
        let mut agents_info = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                let metadata = entry.metadata().await?;
                let created = metadata.created().or_else(|_| metadata.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                if let Ok(name) = entry.file_name().into_string() {
                    agents_info.push((name, created));
                }
            }
        }

        // Sort descending by creation time
        agents_info.sort_by(|a, b| b.1.cmp(&a.1));

        let mut result = Vec::new();
        let paginated_agents = agents_info.into_iter().skip(offset).take(limit);

        for (agent_id, _) in paginated_agents {
            match self.load(&agent_id).await {
                Ok(agent) => result.push(agent),
                Err(e) => {
                    wd_log::log_warn_ln!("[SingleAgentCtlFromFile::{}] skip load agent {}: {}", self.id(), agent_id, e);
                }
            }
        }

        Ok(result)
    }

    async fn create(
        &self,
        _agent_ctl_id: &str,
        agent_id: &str,
        mut cfg: Box<dyn AgentConfig + Send + 'static>,
    ) -> anyhow::Result<AgentRef> {

        let agent_dir = self.workspace_dir.join(agent_id);
        if !agent_dir.exists() {
            tokio::fs::create_dir_all(&agent_dir).await?;
        }

        cfg.init(agent_id, self.workspace_dir.display().to_string().as_str(), Value::Null).await?;

        self.load(agent_id).await
    }

    async fn exit(&self) -> anyhow::Result<()> {
        let cache = self.agents.read().await;
        for agent in cache.values() {
            agent.exit().await;
        }
        Ok(())
    }
}
