use std::any::Any;
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::{Env, Event, Task};

/// 上下文，用于存储智能体执行过程中的状态
#[derive(Debug, Clone)]
pub struct Context {
    inner: Arc<Mutex<Box<dyn Any + Send + Sync + 'static>>>,
}

impl Context {
    /// 创建新的上下文
    pub fn new<T: Any + Send + Sync + 'static>(data: T) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Box::new(data))),
        }
    }
}

/// 构建规划
#[async_trait::async_trait]
pub trait AgentPlanningBuilder {
    fn build(&mut self, env: Env, event: Event) -> anyhow::Result<Arc<dyn AgentPlanning+Send+'static>>;
}

/// 智能体规划 trait，定义智能体的规划逻辑
#[async_trait::async_trait]
pub trait AgentPlanning:Sync {
    /// 下一步规划
    /// Error::PlanOver 表示规划完成
    async fn next(&mut self, event: Event) -> anyhow::Result<Vec<Task>>;
}