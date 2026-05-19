use crate::engine::{AgentLoader, RecallAgentRef};
use crate::workspace::{Workspace, WorkspaceStatus};
use fae_agent::{AgentRef, Env, Environment};
use std::sync::Arc;

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
        //先load新
        if let Ok(o) = self.n.load(agent_id).await {
            Ok(o)
        } else {
            //再load旧
            self.o.load(agent_id).await
        }
    }

    async fn recall(&self, task_desc: &str) -> anyhow::Result<Vec<RecallAgentRef>> {
        //先recall新
        if let Ok(o) = self.n.recall(task_desc).await {
            Ok(o)
        } else {
            //再recall旧
            self.o.recall(task_desc).await
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
    pub fn new<N, L>(name: N, loader: L, env: Env) -> Self
    where
        N: Into<String>,
        L: AgentLoader + Send + 'static,
    {
        WorkspaceBuilder {
            name: name.into(),
            loader: Arc::new(loader),
            env,
        }
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
    pub async fn add_env_layer(&mut self, mut layer: impl Environment + Send + 'static) -> &mut Self {
        let env = self.env.clone();
        self.env = {
            layer.register_parent_env(env).await;
            Env::new(layer)
        };
        self
    }
}
