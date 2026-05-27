use crate::workspace::{Workspace, WorkspaceStatus};
use crate::{AgentLoader, RecallAgentRef, SingleAgentLoaderFromFile};
use fae_agent::{AgentRef, Env, Environment, Error};
use std::sync::Arc;
use crate::workspace::workspace_runtime::WorkspaceRuntime;

pub struct AgentLoaderLayer<T> {
    o: Arc<dyn AgentLoader + Send + 'static>,
    n: T,
}
impl<T> AgentLoaderLayer<T> {
    pub fn new(o: Arc<dyn AgentLoader + Send + 'static>, t: T) -> Self {
        AgentLoaderLayer { o, n: t }
    }
}

#[async_trait::async_trait]
impl<T> AgentLoader for AgentLoaderLayer<T>
where
    T: AgentLoader + Send + 'static,
{
    async fn load(&self, agent_id: &str) -> anyhow::Result<AgentRef> {
        match self.n.load(agent_id).await {
            Ok(o) => Ok(o),
            Err(e) => {
                if let Some(Error::NoSupport) = e.downcast_ref::<Error>() {
                    self.o.load(agent_id).await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn recall(&self, task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>> {
        match self.n.recall(task_desc).await {
            Ok(o) => Ok(o),
            Err(e) => {
                if let Some(Error::NoSupport) = e.downcast_ref::<Error>() {
                    self.o.recall(task_desc).await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn create(
        &self,
        name: &str,
        prompt: &str,
        cfg: &mut Box<dyn std::any::Any + Send + Sync + 'static>,
    ) -> anyhow::Result<AgentRef> {
        match self.n.create(name, prompt, cfg).await {
            Ok(o) => Ok(o),
            Err(e) => {
                if let Some(Error::NoSupport) = e.downcast_ref::<Error>() {
                    self.o.create(name, prompt, cfg).await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn exit(&self) -> anyhow::Result<()> {
        if let Err(err) = self.n.exit().await {
            wd_log::log_error_ln!("[AgentLoaderLayer] exit new loader error: {:?}", err);
        }
        self.o.exit().await
    }
}

pub struct WorkspaceBuilder {
    pub(crate) name: String,
    pub(crate) loader: Arc<dyn AgentLoader + Send + 'static>,
    pub(crate) env: Env,
}
impl WorkspaceBuilder {
    pub fn new<N>(name: N, env: Env) -> Self
    where
        N: Into<String>,
    {
        let name = name.into();

        WorkspaceBuilder {
            name,
            loader: Arc::new(()),
            env,
        }
    }
    pub async fn default_init(mut self) ->Self{
        let single_agent_loader = SingleAgentLoaderFromFile::new(self.name.as_str());
        self.set_loader(single_agent_loader);

        let workspace_env = WorkspaceRuntime::new(self.name.clone());
        self.add_env_layer(workspace_env).await;
        self
    }
    pub fn build(self) -> Workspace {
        let ws = Workspace {
            status: WorkspaceStatus::default(),
            name: self.name,
            loader: self.loader,
            env: self.env,
        };
        ws.start_watch_env();
        ws
    }
    pub fn set_name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = name.into();
        self
    }
    pub fn set_loader(&mut self, loader: impl AgentLoader + Send + 'static) -> &mut Self {
        self.loader = Arc::new(loader);
        self
    }
    pub fn add_loader_layer(&mut self, layer: impl AgentLoader + Send + 'static) -> &mut Self {
        let loader = self.loader.clone();
        self.set_loader(AgentLoaderLayer::new(loader, layer))
    }
    pub async fn add_env_layer(
        &mut self,
        mut layer: impl Environment + Send + 'static,
    ) -> &mut Self {
        let env = self.env.clone();
        self.env = {
            layer.register_parent_env(env).await;
            Env::new(layer)
        };
        self
    }
}