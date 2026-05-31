use wd_tools::PFErr;
use fae_agent::{AgentConfig, AgentRef, SessionMD, SessionMetadata};
use crate::{SingleAgentCtlFromFile, Workspace};

impl Workspace {
    // ---------------- agent 相关 ----------------
    pub async fn get_agent(&self, agent_id: &str) -> anyhow::Result<AgentRef> {
        self.loader.load(agent_id).await
    }
    pub async fn list_agents(&self, limit: usize, offset: usize) -> anyhow::Result<Vec<AgentRef>> {
        self.loader.list(limit, offset).await
    }
    pub async fn create_single_agent<Cfg: AgentConfig + Send + 'static>(
        &self,
        agent_id: &str,
        cfg: Cfg,
    ) -> anyhow::Result<AgentRef> {
        self.create_agent(SingleAgentCtlFromFile::get_id(), agent_id, cfg).await
    }
    pub async fn create_agent<Cfg: AgentConfig + Send + 'static>(
        &self,
        agent_ctl_id: &str,
        agent_id: &str,
        cfg: Cfg,
    ) -> anyhow::Result<AgentRef> {
        self.loader.create(agent_ctl_id, agent_id, Box::new(cfg)).await
    }
    // ---------------- session 相关 ----------------
    pub async fn session_history<M: SessionMetadata+Send+Sync+'static>(&self, agent_id: &str,user_id: &str, limit: usize) -> anyhow::Result<Vec<M>> {
        let list = self.get_agent(agent_id).await?.on_session_ctl().await.list(user_id, 0, limit).await?;
        let mut vec = Vec::with_capacity(list.len());
        for meta in list {
            match meta.into_inner() {
                Ok(meta) => vec.push(meta),
                Err(e) => {
                    return anyhow::anyhow!("Failed to deserialize session meta: {:?}", e).err();
                }
            }
        }
        Ok(vec)
    }
    pub async fn session_reset(&self, agent_id: &str,user_id:&str, session_id:&str) -> anyhow::Result<()> {
        self.get_agent(agent_id).await?.on_memory().await.reset(user_id, session_id).await
    }
    
}
