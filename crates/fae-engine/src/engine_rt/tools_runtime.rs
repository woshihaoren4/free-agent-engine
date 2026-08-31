use std::collections::HashMap;
use std::sync::Arc;

use fae_agent::{
    ContextNull, Ctx, Event, EventType, RuntimeSelectExec, TaskError, TaskReq, TaskResp, TaskType,
    ToolRequest, ToolResponse, Tools,
};
use serde_json::Value;
use wd_tools::channel::{Channel, Receiver, Sender};

const DEFAULT_TOOL_CHANNEL: &str = "default";

#[derive(Debug)]
pub struct ToolsRuntime {
    tools: HashMap<String, Arc<dyn Tools>>,
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Default for ToolsRuntime {
    fn default() -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            tools: HashMap::new(),
            event_sender,
            event_receiver,
        }
    }
}

impl ToolsRuntime {
    pub const ID: &'static str = "tools_default";

    pub fn new() -> Self {
        Self::default()
    }

    pub fn tools(&self) -> &HashMap<String, Arc<dyn Tools>> {
        &self.tools
    }

    pub fn tool(&self, tool_name: &str) -> Option<&dyn Tools> {
        self.lookup_tool(tool_name).map(Arc::as_ref)
    }

    pub fn contains_tool(&self, tool_name: &str) -> bool {
        self.lookup_tool(tool_name).is_some()
    }

    pub fn add_tool(&mut self, tool: Box<dyn Tools>) -> Option<Arc<dyn Tools>> {
        self.tools.insert(tool.channel().to_string(), tool.into())
    }

    pub fn add_tool_with_channel(
        &mut self,
        channel: impl Into<String>,
        tool: Box<dyn Tools>,
    ) -> Option<Arc<dyn Tools>> {
        self.tools.insert(channel.into(), tool.into())
    }

    pub fn remove_tool(&mut self, channel: &str) -> Option<Arc<dyn Tools>> {
        self.tools.remove(channel)
    }

    fn tool_channel(tool_name: &str) -> &str {
        tool_name
            .split_once("__")
            .map(|(channel, _)| channel)
            .unwrap_or(tool_name)
    }

    fn lookup_tool(&self, tool_name: &str) -> Option<&Arc<dyn Tools>> {
        if let Some(tool) = self.tools.get(Self::tool_channel(tool_name)) {
            return Some(tool);
        }

        if !tool_name.contains("__") {
            return self.tools.get(DEFAULT_TOOL_CHANNEL);
        }

        None
    }

    async fn exec_tool(
        &self,
        task: TaskReq<ToolRequest>,
    ) -> fae_agent::Result<TaskResp<ToolResponse>> {
        let TaskReq { ctx, meta, req } = task;
        let tool = self
            .lookup_tool(req.get_tool_name())
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;
        let resp = tool.exec(&ctx, req).await?;

        Ok(TaskResp { ctx, meta, resp })
    }
}

#[async_trait::async_trait]
impl RuntimeSelectExec<ToolRequest, ToolResponse, String, Value> for ToolsRuntime {
    fn id(&self) -> &str {
        Self::ID
    }

    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Tool]
    }

    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.event_receiver.clone())
    }

    async fn select(&self, ty: TaskType, tool_name: String) -> fae_agent::Result<Value> {
        if ty != TaskType::Tool {
            return Err(fae_agent::Error::RuntimeNoSupport);
        }

        let tool = self
            .lookup_tool(&tool_name)
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;
        let ctx = Ctx::new(Arc::new(ContextNull));
        Ok(tool.desc(&ctx, &tool_name).await?)
    }

    async fn spawn(&self, task: TaskReq<ToolRequest>) -> fae_agent::Result<()> {
        let TaskReq { ctx, mut meta, req } = task;
        let tool = self
            .lookup_tool(req.get_tool_name())
            .cloned()
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;
        let event_sender = self.event_sender.clone();

        tokio::spawn(async move {
            let runtime_id = Self::ID.to_string();
            if meta.publisher.is_empty() {
                meta.publisher = runtime_id.clone();
            }
            let response_ctx = ctx.clone();
            let error_meta = meta.clone();
            let result = tool.exec(&ctx, req).await.map(|resp| {
                let response = TaskResp {
                    ctx: response_ctx,
                    meta,
                    resp,
                }
                .into_response();
                Event {
                    from_rt_id: runtime_id.clone(),
                    event_type: EventType::TaskResult(response),
                }
            });

            match result {
                Ok(event) => {
                    if let Err(err) = event_sender.send(event).await {
                        wd_log::log_error_ln!("send tool task result failed: {:?}", err);
                    }
                }
                Err(error) => {
                    let event = Event {
                        from_rt_id: runtime_id,
                        event_type: EventType::TaskError(TaskError {
                            ctx,
                            meta: error_meta,
                            error: error.to_string(),
                        }),
                    };
                    if let Err(error) = event_sender.send(event).await {
                        wd_log::log_error_ln!("send tool task error failed: {:?}", error);
                    }
                }
            }
        });

        Ok(())
    }

    async fn exec(&self, task: TaskReq<ToolRequest>) -> fae_agent::Result<TaskResp<ToolResponse>> {
        self.exec_tool(task).await
    }
}
