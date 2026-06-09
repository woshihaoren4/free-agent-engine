use async_trait::async_trait;
use fae_agent::{
    Context, Error, Select, TaskExecutorExt, Thing, ThingItem, ThingSelect, ToolRequest,
    ToolResponse,
};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
// pub trait Identity:Sync{
//     fn get(&self) -> String;
//     fn user_id(&self) -> String;
// }

#[derive(Debug)]
pub struct IdenInfo {
    pub task_id: String,
    pub agent_id: String,
    pub user_id: String,
    pub ctx: Context, // pub identity: Box<dyn Identity+Send+'static>,
}
impl IdenInfo {
    pub fn new(task_id: String, agent_id: String, user_id: String, ctx: Context) -> Self {
        Self {
            task_id,
            agent_id,
            user_id,
            ctx,
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

    pub fn get(&self, key: &str) -> Option<String> {
        self.ctx.get(key)
    }
    pub fn set(&mut self, key: String, value: String) {
        self.ctx.set(key, value);
    }
}

#[async_trait]
pub trait Tool: Debug + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn arguments(&self) -> Value;
    async fn call(&self, iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse>;
}

#[async_trait]
pub trait ToolSet: Debug + Sync {
    async fn load(&self, name: &str) -> anyhow::Result<Arc<dyn Tool + Send + 'static>>;
    async fn insert(&mut self, tool: Arc<dyn Tool + Send + 'static>) -> anyhow::Result<()>;
}

#[derive(Default, Debug)]
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
            Err(anyhow::anyhow!("tools not found: {}", name))
        }
    }

    async fn insert(&mut self, tool: Arc<dyn Tool + Send + 'static>) -> anyhow::Result<()> {
        self.tools.insert(tool.name().to_string(), tool);
        Ok(())
    }
}

#[derive(Debug)]
pub struct ToolExecutor {
    pub tools_loader: Vec<Box<dyn ToolSet + Send + 'static>>,
}

#[async_trait::async_trait]
impl TaskExecutorExt<ToolRequest, ToolResponse> for ToolExecutor {
    fn desc(&self) -> String {
        "default tools executor".to_string()
    }

    fn channel(&self) -> String {
        "default".to_string()
    }
    async fn exec(
        &self,
        ctx: Context,
        task_id: String,
        agent_id: String,
        user_id: String,
        req: ToolRequest,
    ) -> anyhow::Result<ToolResponse> {
        let tool = self.load_tool(req.get_tool_name()).await?;
        let result = tool
            .call(
                IdenInfo::new(task_id, agent_id, user_id, ctx),
                req.arguments,
            )
            .await;
        match result {
            Ok(resp) => Ok(resp),
            Err(e) => {
                let info = format!("Tool[{}] call failed. error: {}", tool.name(), e);
                Ok(ToolResponse::with_result(info))
            }
        }
    }
    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>> {
        if let ThingSelect::Tool(_, name) = select.select {
            let tool = self.load_tool(name.as_str()).await?;
            let thing = Thing::new(self.channel())
                .add_item(ThingItem::Tool(
                    tool.description().to_string(),
                    tool.arguments(),
                ))
                .into_self();
            return Ok(vec![thing]);
        }
        return Err(Error::NoSupport.into());
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
        return Err(anyhow::anyhow!("[ToolExecutor] tools not found: {}", name));
    }
}

impl Default for ToolExecutor {
    fn default() -> Self {
        let mut tools = HashMap::new();

        let tool_list: Vec<Arc<dyn Tool + Send + 'static>> = vec![
            Arc::new(crate::tools::ExecuteCommand::default()),
            Arc::new(crate::tools::SendHttpRequest),
            Arc::new(crate::tools::ReadFile),
            Arc::new(crate::tools::WriteFile::default()),
            Arc::new(crate::tools::ListDirectory),
            Arc::new(crate::tools::ExecutePython),
            Arc::new(crate::tools::TodoWrite::default()),
            Arc::new(crate::tools::ArkWebSearch::default()),
        ];

        for tool in tool_list {
            tools.insert(tool.name().to_string(), tool);
        }

        Self {
            tools_loader: vec![Box::new(ToolSetImplMap { tools })],
        }
    }
}
