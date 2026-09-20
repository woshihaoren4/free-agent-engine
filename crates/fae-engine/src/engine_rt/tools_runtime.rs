use std::collections::HashMap;
use std::sync::Arc;

use fae_agent::{
    ContextNull, Ctx, Event, EventType, RuntimeSelectExec, TaskError, TaskReq, TaskResp, TaskType,
    ToolRequest, ToolResponse, Tools, hook::tools_hook::ToolsHookBuilder,
};
use serde_json::Value;
use wd_tools::channel::{Channel, Receiver, Sender};

const DEFAULT_TOOL_CHANNEL: &str = "default";

pub struct ToolsRuntime {
    tools: HashMap<String, Arc<dyn Tools>>,
    tools_hooks: Vec<Box<dyn ToolsHookBuilder>>,
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl std::fmt::Debug for ToolsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolsRuntime")
            .field("tools", &self.tools)
            .field("tools_hook_count", &self.tools_hooks.len())
            .field("event_sender", &self.event_sender)
            .field("event_receiver", &self.event_receiver)
            .finish()
    }
}

impl Default for ToolsRuntime {
    fn default() -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            tools: HashMap::new(),
            tools_hooks: Vec::new(),
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

    pub fn tools_hooks(&self) -> &[Box<dyn ToolsHookBuilder>] {
        &self.tools_hooks
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

    pub fn add_tools_hook<H>(&mut self, hook: H)
    where
        H: ToolsHookBuilder,
    {
        self.tools_hooks.push(Box::new(hook));
    }

    pub fn add_tools_hook_box(&mut self, hook: Box<dyn ToolsHookBuilder>) {
        self.tools_hooks.push(hook);
    }

    pub fn remove_tools_hooks(&mut self) -> Vec<Box<dyn ToolsHookBuilder>> {
        std::mem::take(&mut self.tools_hooks)
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

    async fn build_tool(&self, tool_name: &str) -> Option<Arc<dyn Tools>> {
        let mut tool = self.lookup_tool(tool_name)?.clone();
        for hook in &self.tools_hooks {
            tool = hook.build(tool).await;
        }
        Some(tool)
    }

    async fn exec_tool(
        &self,
        task: TaskReq<ToolRequest>,
    ) -> fae_agent::Result<TaskResp<ToolResponse>> {
        let TaskReq { ctx, meta, req } = task;
        let tool = self
            .build_tool(req.get_tool_name())
            .await
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
            .build_tool(&tool_name)
            .await
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;
        let ctx = Ctx::new(Arc::new(ContextNull));
        Ok(tool.desc(&ctx, &tool_name).await?)
    }

    async fn spawn(&self, task: TaskReq<ToolRequest>) -> fae_agent::Result<()> {
        let TaskReq { ctx, mut meta, req } = task;
        let tool = self
            .build_tool(req.get_tool_name())
            .await
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

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::{TaskMeta, hook::tools_hook::ToolsHookBuilder};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct CountingTools {
        execs: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Tools for CountingTools {
        fn channel(&self) -> &str {
            "test"
        }

        async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
            Ok(serde_json::json!({ "name": tool_name }))
        }

        async fn exec(&self, _ctx: &Ctx, _req: ToolRequest) -> anyhow::Result<ToolResponse> {
            self.execs.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResponse::with_result("ok".to_string()))
        }
    }

    struct CountingToolsHookBuilder {
        builds: Arc<AtomicUsize>,
        descriptions: Arc<AtomicUsize>,
        executions: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ToolsHookBuilder for CountingToolsHookBuilder {
        async fn build(&self, tools: Arc<dyn Tools>) -> Arc<dyn Tools> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            Arc::new(CountingToolsHook {
                tools,
                descriptions: self.descriptions.clone(),
                executions: self.executions.clone(),
            })
        }
    }

    #[derive(Debug)]
    struct CountingToolsHook {
        tools: Arc<dyn Tools>,
        descriptions: Arc<AtomicUsize>,
        executions: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Tools for CountingToolsHook {
        fn channel(&self) -> &str {
            self.tools.channel()
        }

        async fn desc(&self, ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
            self.descriptions.fetch_add(1, Ordering::SeqCst);
            self.tools.desc(ctx, tool_name).await
        }

        async fn exec(&self, ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            self.tools.exec(ctx, req).await
        }
    }

    fn tool_task(id: &str) -> TaskReq<ToolRequest> {
        TaskReq {
            ctx: Ctx::new(Arc::new(ContextNull)),
            meta: TaskMeta {
                id: id.to_string(),
                ty: TaskType::Tool,
                ..Default::default()
            },
            req: ToolRequest::new("test__echo".to_string(), "{}".to_string()),
        }
    }

    #[tokio::test]
    async fn builds_tools_hooks_for_every_operation() {
        let builds = Arc::new(AtomicUsize::new(0));
        let descriptions = Arc::new(AtomicUsize::new(0));
        let hook_executions = Arc::new(AtomicUsize::new(0));
        let tool_executions = Arc::new(AtomicUsize::new(0));
        let mut runtime = ToolsRuntime::new();
        runtime.add_tool(Box::new(CountingTools {
            execs: tool_executions.clone(),
        }));
        runtime.add_tools_hook(CountingToolsHookBuilder {
            builds: builds.clone(),
            descriptions: descriptions.clone(),
            executions: hook_executions.clone(),
        });

        runtime
            .select(TaskType::Tool, "test__echo".to_string())
            .await
            .unwrap();
        runtime.exec(tool_task("exec")).await.unwrap();

        let receiver = runtime.watch().await.unwrap();
        runtime.spawn(tool_task("spawn")).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .expect("spawned tool did not produce an event")
            .unwrap();

        assert_eq!(builds.load(Ordering::SeqCst), 3);
        assert_eq!(descriptions.load(Ordering::SeqCst), 1);
        assert_eq!(hook_executions.load(Ordering::SeqCst), 2);
        assert_eq!(tool_executions.load(Ordering::SeqCst), 2);
    }
}
