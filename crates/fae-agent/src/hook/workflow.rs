use std::fmt::Debug;

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
pub trait WorkflowHook: Debug + Send + Sync + 'static {
    async fn on_start(&self, _context: WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_parallel_start(&self, _context: WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_execute(&self, _context: WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_decision(&self, _context: WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_loop(&self, _context: WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_end(&self, _context: WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_join_end(&self, _context: WorkflowHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }
}
