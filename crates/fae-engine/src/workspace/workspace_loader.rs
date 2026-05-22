use crate::{AgentLoader, RecallAgentRef};
use fae_agent::{AgentConfigFile, AgentEventHandleImpl, FileChatMemory, FileSessionMetaManager};
use fae_agent::{AgentRef, SingleAgent, SingleAgentSessionConfig};
use std::path::PathBuf;
use std::sync::Arc;

use std::collections::HashMap;
use tokio::sync::RwLock;

/// A loader that loads agents from a workspace directory structure.
pub struct SingleAgentLoaderFromFile {
    workspace_dir: PathBuf,
    agents: RwLock<HashMap<String, AgentRef>>,
}

impl SingleAgentLoaderFromFile {
    pub fn new<P: Into<PathBuf>>(workspace_dir: P) -> Self {
        Self {
            workspace_dir: workspace_dir.into(),
            agents: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl AgentLoader for SingleAgentLoaderFromFile {
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
                "Agent directory not found: {:?}",
                agent_dir
            ));
        }

        let session_dir = agent_dir.join("session");
        let config_file = agent_dir.join("config.json");

        // 1. Session memory
        let memory = FileChatMemory::<String>::new(&session_dir).await?;

        // 2. Session config
        let session_config = FileSessionMetaManager::<SingleAgentSessionConfig>::new(
            &session_dir,
            |meta| meta.id.clone(),
            |_| 0, // SingleAgentSessionConfig doesn't have updated_at, return 0
        )
        .await?;

        // 3. Agent config
        let agent_config = AgentConfigFile::load_or_default(&config_file).await?;

        // 4. Create the agent
        let single_agent = SingleAgent::<String>::new(
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
        Ok(vec![])
    }

    async fn create(
        &self,
        name: &str,
        prompt: &str,
        cfg: Box<dyn std::any::Any + Send + Sync + 'static>,
    ) -> anyhow::Result<AgentRef> {
        let mut agent_config_data = match cfg.downcast::<fae_agent::AgentConfigData>() {
            Ok(data) => *data,
            Err(_) => return Err(anyhow::anyhow!("CreateAgentNotSupported, name: {}", name)),
        };

        let agent_dir = self.workspace_dir.join(name);
        if !agent_dir.exists() {
            tokio::fs::create_dir_all(&agent_dir).await?;
        }

        let session_dir = agent_dir.join("session");
        if !session_dir.exists() {
            tokio::fs::create_dir_all(&session_dir).await?;
        }

        let config_file = agent_dir.join("config.json");
        let prompt_file = agent_dir.join("prompt.txt");

        agent_config_data.prompt_dir = agent_dir.to_string_lossy().to_string();

        let config_json = serde_json::to_string_pretty(&agent_config_data)?;
        tokio::fs::write(&config_file, config_json).await?;
        tokio::fs::write(&prompt_file, prompt).await?;

        self.load(name).await
    }

    async fn exit(&self) -> anyhow::Result<()> {
        let cache = self.agents.read().await;
        for agent in cache.values() {
            agent.exit().await;
        }
        Ok(())
    }
}
