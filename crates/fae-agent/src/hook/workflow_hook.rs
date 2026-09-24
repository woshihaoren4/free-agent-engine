use std::{fmt::Debug, sync::Arc};

use serde_json::Value;

use crate::{Ctx, WorkflowNode};

#[derive(Debug, Clone, Copy)]
pub enum WorkflowHookPhase<'a> {
    Before,
    After { output: Option<&'a Value> },
    Failed { error: &'a str },
}

#[derive(Debug)]
pub struct WorkflowHookContext<'a> {
    pub ctx: &'a Ctx,
    pub plan_id: &'a str,
    pub workflow_id: &'a str,
    pub node_id: &'a str,
    pub node: &'a WorkflowNode,
    pub phase: WorkflowHookPhase<'a>,
}

#[async_trait::async_trait]
pub trait WorkflowHookBuilder: Send + Sync + 'static {
    async fn build(&self) -> Arc<dyn WorkflowHook>;
}

#[derive(Debug, Default)]
pub(crate) struct CompositeWorkflowHook {
    hooks: Vec<Arc<dyn WorkflowHook>>,
}

impl CompositeWorkflowHook {
    pub(crate) fn new(hooks: Vec<Arc<dyn WorkflowHook>>) -> Self {
        Self { hooks }
    }
}

#[async_trait::async_trait]
pub trait WorkflowHook: Debug + Send + Sync + 'static {
    async fn on_start(&self, _ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_parallel_start(&self, _ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_execute(&self, _ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_decision(&self, _ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_loop(&self, _ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_end(&self, _ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_join_end(&self, _ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl WorkflowHook for CompositeWorkflowHook {
    async fn on_start(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        for hook in &self.hooks {
            hook.on_start(ctx).await?;
        }
        Ok(())
    }

    async fn on_parallel_start(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        for hook in &self.hooks {
            hook.on_parallel_start(ctx).await?;
        }
        Ok(())
    }

    async fn on_execute(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        for hook in &self.hooks {
            hook.on_execute(ctx).await?;
        }
        Ok(())
    }

    async fn on_decision(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        for hook in &self.hooks {
            hook.on_decision(ctx).await?;
        }
        Ok(())
    }

    async fn on_loop(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        for hook in &self.hooks {
            hook.on_loop(ctx).await?;
        }
        Ok(())
    }

    async fn on_end(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        for hook in &self.hooks {
            hook.on_end(ctx).await?;
        }
        Ok(())
    }

    async fn on_join_end(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        for hook in &self.hooks {
            hook.on_join_end(ctx).await?;
        }
        Ok(())
    }
}
