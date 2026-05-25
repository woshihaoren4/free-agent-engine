use async_trait::async_trait;
use fae_agent::{
    Error, Task, TaskExecutor, TaskExecutorExt, TaskExecutorExtImpl, TaskResult, ToolRequest,
};
use std::collections::HashMap;
use std::sync::Arc;

// pub trait Identity:Sync{
//     fn get(&self) -> String;
//     fn user_id(&self) -> String;
// }

pub struct IdenInfo {
    pub task_id: String,
    pub agent_id: String,
    pub user_id: String,
    // pub identity: Box<dyn Identity+Send+'static>,
}
impl IdenInfo {
    pub fn new(task_id: String, agent_id: String, user_id: String) -> Self {
        Self {
            task_id,
            agent_id,
            user_id,
        }
    }
    pub fn get_task_id(&self) -> &str {
        &self.task_id
    }
    pub fn get_agent_id(&self) -> &str {
        &self.agent_id
    }
    pub fn get_user_id(&self) -> &str {
        &self.user_id
    }
}

#[async_trait]
pub trait Tool: Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn arguments(&self) -> &str;
    async fn call(&self, iden: IdenInfo, args: String) -> anyhow::Result<String>;
}

#[async_trait]
pub trait ToolSet: Sync {
    async fn load(&self, name: &str) -> anyhow::Result<Arc<dyn Tool + Send + 'static>>;
    async fn insert(&mut self, tool: Arc<dyn Tool + Send + 'static>) -> anyhow::Result<()>;
}

#[derive(Default)]
pub struct ToolSetImplMap {
    tools: HashMap<String, Arc<dyn Tool + Send + 'static>>,
}
#[async_trait]
impl ToolSet for ToolSetImplMap {
    async fn load(&self, name: &str) -> anyhow::Result<Arc<dyn Tool + Send + 'static>> {
        let tool = self.tools.get(name).cloned();
        if let Some(tool) = tool {
            Ok(tool)
        } else {
            Err(anyhow::anyhow!("tool not found: {}", name))
        }
    }

    async fn insert(&mut self, tool: Arc<dyn Tool + Send + 'static>) -> anyhow::Result<()> {
        self.tools.insert(tool.name().to_string(), tool);
        Ok(())
    }
}

pub struct ToolExecutor {
    pub tools_loader: Vec<Box<dyn ToolSet + Send + 'static>>,
}

#[async_trait::async_trait]
impl TaskExecutorExt<ToolRequest, String> for ToolExecutor {
    fn desc(&self) -> String {
        "default tool executor".to_string()
    }

    fn channel(&self) -> String {
        "default".to_string()
    }
    async fn exec(
        &self,
        task_id: String,
        agent_id: String,
        user_id: String,
        req: ToolRequest,
    ) -> anyhow::Result<String> {
        let tool = self.load_tool(req.get_tool_name()).await?;
        let result = tool
            .call(IdenInfo::new(task_id, agent_id, user_id), req.arguments)
            .await?;
        Ok(result)
    }
}

impl ToolExecutor {
    pub fn add_tools_loader<T: ToolSet + Send + 'static>(mut self, tools_loader: T) -> Self {
        self.tools_loader.push(Box::new(tools_loader));
        self
    }
    pub async fn add_tool_at_last<T: Tool + Send + 'static>(mut self, tool: T) -> Self {
        self.tools_loader
            .last_mut()
            .unwrap()
            .insert(Arc::new(tool))
            .await
            .unwrap();
        self
    }
    pub async fn load_tool(&self, name: &str) -> anyhow::Result<Arc<dyn Tool + Send + 'static>> {
        for loader in &self.tools_loader {
            match loader.load(name).await {
                Ok(tool) => return Ok(tool),
                Err(e) => {
                    if let Some(Error::NoSupport) = e.downcast_ref::<Error>() {
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        return Err(anyhow::anyhow!("[ToolExecutor] tool not found: {}", name));
    }
}

impl Default for ToolExecutor {
    fn default() -> Self {
        Self {
            tools_loader: vec![Box::new(ToolSetImplMap::default())],
        }
    }
}
