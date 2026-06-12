use crate::workspace::workspace_runtime::WorkspaceRuntime;
use crate::workspace::{Workspace, WorkspaceStatus};
use crate::{AgentCtl, AgentLoaderLayer, RecallAgentRef, SingleAgentCtlFromFile};
use fae_agent::{AgentConfig, AgentRef, Env, Environment, Error};
use std::any::Any;
use std::sync::Arc;

pub struct WorkspaceBuilder {
    pub(crate) name: String,
    pub(crate) loader: Arc<dyn AgentCtl + Send + 'static>,
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
    pub fn get_name(&self) -> &str {
        self.name.as_str()
    }
    pub fn set_loader(&mut self, loader: impl AgentCtl + Send + 'static) -> &mut Self {
        self.loader = Arc::new(loader);
        self
    }
    pub fn add_loader_layer(&mut self, layer: impl AgentCtl + Send + 'static) -> &mut Self {
        let loader = self.loader.clone();
        self.set_loader(AgentLoaderLayer::new(loader, layer))
    }
    pub fn get_loader(&self)->Arc<dyn AgentCtl + Send + 'static>{
        self.loader.clone()
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
