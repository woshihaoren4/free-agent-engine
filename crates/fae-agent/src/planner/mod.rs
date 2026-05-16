mod plan_to_agent;

use std::sync::Arc;
use tokio::sync::Mutex;
use crate::{Env, Event, Session, SessionInfo, Task};

/// 构建规划
#[async_trait::async_trait]
pub trait AgentPlanningExt<T: Planning +Send+'static>:Sync {
    fn id(&self) -> String;
    async fn generate_plan(&self, env: Env, event: &mut Event) -> anyhow::Result<T>;
    async fn exit(&self) -> anyhow::Result<()>;
}

/// 智能体规划 trait，定义智能体的规划逻辑
#[async_trait::async_trait]
pub trait Planning:Sync {
    fn id(&self) -> String;
    /// 下一步规划
    /// Error::PlanOver 表示规划完成
    async fn next(&mut self, event: Event) -> anyhow::Result<Vec<Task>>;
    /// 强制终止
    async fn abort(&mut self) -> anyhow::Result<()>;
}