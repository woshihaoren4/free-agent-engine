use fae_agent::{
    Env, EnvEvent, Environment, Select, TASK_EXTEND_KEY_WORKSPACE, Task, TaskResult, Thing,
    ThingItem, ThingSelect,
};

const DEFAULT_WORKSPACE_RUNTIME_ID: &str = "FAE_DEFAULT_WORKSPACE_RUNTIME";

pub struct WorkspaceRuntime {
    pub name: String,
    parent: Option<Env>,
}

impl WorkspaceRuntime {
    pub fn new(name: String) -> Self {
        Self { name, parent: None }
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
            if let Some(env) = self.get_env_var(key) {
                return Ok(vec![env]);
            }
        }
        if let Some(ref env) = self.parent {
            env.query(select).await
        } else {
            Err(anyhow::anyhow!("[WorkspaceRuntime] env not found"))
        }
    }

    async fn spawn(&self, mut tasks: Vec<Task>) -> anyhow::Result<()> {
        for task in &mut tasks {
            task.set(TASK_EXTEND_KEY_WORKSPACE, self.name.clone());
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
        task.set(TASK_EXTEND_KEY_WORKSPACE, self.name.clone());
        if let Some(parent) = &self.parent {
            parent.execute(task).await
        } else {
            Err(anyhow::anyhow!(
                "[WorkspaceRuntime] parent env not registered"
            ))
        }
    }
}
