use crate::AgentCtl;
use fae_agent::{
    Env, EnvEvent, Environment, FAE_WORKSPACE, GLOBAL_KEY_WORKSPACE, Select, Task, TaskResult,
    Thing, ThingItem, ThingSelect,
};
use std::sync::Arc;

const DEFAULT_WORKSPACE_RUNTIME_ID: &str = "FAE_DEFAULT_WORKSPACE_RUNTIME";

#[derive(Debug)]
pub struct WorkspaceRuntime {
    pub name: String,
    parent: Option<Env>,
    loader: Arc<dyn AgentCtl + Send + 'static>,
}

impl WorkspaceRuntime {
    pub fn new(name: String, loader: Arc<dyn AgentCtl + Send + 'static>) -> Self {
        Self {
            name,
            parent: None,
            loader,
        }
    }
    pub fn get_env_var(&self, name: &str) -> Option<Thing> {
        match name {
            FAE_WORKSPACE => Some(
                Thing::new(self.id().to_string())
                    .add_item(ThingItem::EnvVar(self.name.clone()))
                    .into_self(),
            ),
            other => {
                if let Ok(o) = std::env::var(other) {
                    Some(
                        Thing::new(self.id().to_string())
                            .add_item(ThingItem::EnvVar(o))
                            .into_self(),
                    )
                } else {
                    None
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Environment for WorkspaceRuntime {
    fn id(&self) -> &'static str {
        DEFAULT_WORKSPACE_RUNTIME_ID
    }

    async fn register_parent_env(&mut self, env: Env) {
        self.parent = Some(env);
    }

    async fn watch(&self) -> anyhow::Result<EnvEvent> {
        if let Some(parent) = &self.parent {
            parent.watch().await
        } else {
            Err(anyhow::anyhow!(
                "[WorkspaceRuntime] parent env not registered"
            ))
        }
    }

    async fn query(&self, mut select: Select) -> anyhow::Result<Vec<Thing>> {
        select.workspace = Some(self.name.clone());
        if let ThingSelect::Env(ref key) = select.select {
            if key == FAE_WORKSPACE {
                return Ok(vec![
                    Thing::new(self.id().to_string())
                        .add_item(ThingItem::EnvVar(self.name.clone()))
                        .into_self(),
                ]);
            }
            if let Some(env) = self.get_env_var(key) {
                return Ok(vec![env]);
            }
        } else if let ThingSelect::Agent(id) = select.select {
            let agent_ref = self.loader.load(id.as_str()).await?;
            let thing = Thing::new(self.id().to_string())
                .add_item(ThingItem::Agent(id, agent_ref.desc()))
                .into_self();
            return Ok(vec![thing]);
        }
        if let Some(ref env) = self.parent {
            env.query(select).await
        } else {
            Err(anyhow::anyhow!("[WorkspaceRuntime] env not found"))
        }
    }

    async fn spawn(&self, mut tasks: Vec<Task>) -> anyhow::Result<()> {
        for task in &mut tasks {
            task.set(GLOBAL_KEY_WORKSPACE, self.name.clone());
        }
        if let Some(parent) = &self.parent {
            parent.spawn(tasks).await
        } else {
            Err(anyhow::anyhow!(
                "[WorkspaceRuntime] parent env not registered"
            ))
        }
    }

    async fn execute(&self, mut task: Task) -> anyhow::Result<TaskResult> {
        task.set(GLOBAL_KEY_WORKSPACE, self.name.clone());
        if let Some(parent) = &self.parent {
            parent.execute(task).await
        } else {
            Err(anyhow::anyhow!(
                "[WorkspaceRuntime] parent env not registered"
            ))
        }
    }
}
