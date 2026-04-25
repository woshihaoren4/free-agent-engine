use std::sync::Arc;
use fae_agent::{Env, Environment};
use crate::engine::AgentLoader;

#[derive(Clone)]
pub struct Workspace {
    pub(crate) name: String,
    pub(crate) loader: Arc<dyn AgentLoader + Send + 'static>,
    pub(crate) env : Env,
}

pub struct WorkspaceBuilder {
    pub(crate) name: String,
    pub(crate) loader: Arc<dyn AgentLoader + Send + 'static>,
    pub(crate) env : Env,
}
impl WorkspaceBuilder {
    pub fn new<N,L>(name:N, loader: L, env:Env) -> Self
    where
        N:Into<String>,
        L: AgentLoader+Send+'static,
    {
        WorkspaceBuilder { name: name.into(), loader: Arc::new(loader), env}
    }
    pub fn build(self) -> Workspace {
        Workspace { name: self.name, loader: self.loader, env: self.env }
    }
    pub fn set_name(mut self, name:impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
    pub fn set_loader(mut self, loader: impl AgentLoader+Send+'static) -> Self {
        self.loader = Arc::new(loader);
               self
    }
    pub async fn add_env_layer(&mut self, mut layer: impl Environment+Send+'static) -> &mut Self {
        self.env = {
            layer.register_parent_env(self.env.clone()).await;
            Env::new(layer)
        };
        self
    }
}