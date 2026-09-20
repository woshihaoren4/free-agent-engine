use std::fmt::Debug;

use crate::{Ctx, SingleAgentInfo};

#[derive(Debug, Clone, Copy)]
pub enum SingleAgentHookPhase<'a> {
    Before,
    After,
    Failed { error: &'a str },
}

#[derive(Debug)]
pub struct SingleAgentHookContext<'a> {
    pub ctx: &'a Ctx,
    pub plan_id: &'a str,
    pub turn_id: u64,
    pub agent: &'a SingleAgentInfo,
    pub task_id: Option<&'a str>,
    pub phase: SingleAgentHookPhase<'a>,
}

#[async_trait::async_trait]
pub trait SingleAgentHook: Debug + Send + Sync + 'static {
    async fn on_memory(&self, _context: SingleAgentHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_history(&self, _context: SingleAgentHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_compression(&self, _context: SingleAgentHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_model(&self, _context: SingleAgentHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_tools(&self, _context: SingleAgentHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_save(&self, _context: SingleAgentHookContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }
}
