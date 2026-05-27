use fae_agent::{Env, EnvEvent, Environment, Task, TaskResult, Thing, ThingItem, ThingSelect, FAE_HOME};

const DEFAULT_WORKSPACE_RUNTIME_ID: &str = "FAE_DEFAULT_WORKSPACE_RUNTIME";

pub struct WorkspaceRuntime{
    pub name:String,
    pub fae_home:String,
    parent: Option<Env>,
}

impl WorkspaceRuntime {
    pub fn new(name:String) -> Self {
        let fae_home = std::env::var(FAE_HOME).unwrap_or("".to_string());
        Self{name,fae_home,parent: None}
    }
    pub fn get_env_var(&self, name:&str)-> Option<Thing>  {
        match name {
            FAE_HOME => {
                Some(Thing::new(self.id().to_string())
                    .add_item(ThingItem::EnvVar(self.fae_home.clone())).into_self())
            }
            FAE_WORKSPACE => {
                Some(Thing::new(self.id().to_string())
                    .add_item(ThingItem::EnvVar(self.name.clone())).into_self())
            }
            other => {
                if let Ok(o) = std::env::var(other){
                    Some(Thing::new(self.id().to_string())
                        .add_item(ThingItem::EnvVar(o)).into_self())
                } else {
                    None
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Environment for WorkspaceRuntime{
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
            Err(anyhow::anyhow!("[WorkspaceRuntime] parent env not registered"))
        }
    }

    async fn query(&self, select: ThingSelect) -> anyhow::Result<Vec<Thing>> {
        if let ThingSelect::Env(ref key ) = select{
            if let Some(env) = self.get_env_var(key) {
                return Ok(vec![env])
            }
        }
        if let Some(ref env) = self.parent {
            env.query(select).await
        } else {
            Err(anyhow::anyhow!("[WorkspaceRuntime] env not found"))
        }
    }

    async fn spawn(&self, tasks: Vec<Task>) -> anyhow::Result<()> {
        if let Some(parent) = &self.parent {
            parent.spawn(tasks).await
        } else {
            Err(anyhow::anyhow!("[WorkspaceRuntime] parent env not registered"))
        }
    }

    async fn execute(&self, task: Task) -> anyhow::Result<TaskResult> {
        if let Some(parent) = &self.parent {
            parent.execute(task).await
        } else {
            Err(anyhow::anyhow!("[WorkspaceRuntime] parent env not registered"))
        }
    }
}

