mod agent_event_handle;

pub use agent_event_handle::*;

use crate::define::Event;
use crate::{Context, Env, Task, TaskResult};

/// 构建规划
#[async_trait::async_trait]
pub trait AgentPlanningExt<T: Planning + Send + 'static>: Sync {
    fn id(&self) -> String;
    async fn to_plan(&self, env: Env, event: Event) -> anyhow::Result<T>;
    async fn exit(&self);
}

#[derive(Debug)]
pub enum PlanningResult {
    /// 任务完成,none:不告诉agent，some：告诉agent任务完成结果
    End(Option<TaskResult>),
    Tasks(Vec<Task>),
}
impl PlanningResult {
    pub fn is_end(&self) -> bool {
        matches!(self, PlanningResult::End(_))
    }
}

/// 智能体规划 trait，定义智能体的规划逻辑
#[async_trait::async_trait]
pub trait Planning: Sync {
    fn id(&self) -> String;
    /// 执行信息，辅助debug
    async fn debug(&self) -> String {
        format!("[{}] running...", self.id())
    }
    /// 初始化规划
    async fn init(&mut self) -> anyhow::Result<PlanningResult>;
    /// 下一步规划
    async fn next(&mut self, event: TaskResult) -> anyhow::Result<PlanningResult>;
    /// 强制终止
    async fn abort(&mut self) {
        wd_log::log_error_ln!(
            "[Planning]::{} aborted, debug info:{}",
            self.id(),
            self.debug().await
        );
    }
    fn get_context(&self) -> Context;
}

// 聚合若干个实现Planning的结构同时实现planing
#[macro_export]
macro_rules! define_planning_group {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident($inner:ty)
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                $variant($inner),
            )*
        }

        #[async_trait::async_trait]
        impl $crate::planner::Planning for $name {
            fn id(&self) -> String {
                match self {
                    $( Self::$variant(inner) => inner.id(), )*
                }
            }

            async fn init(&mut self) -> anyhow::Result<$crate::planner::PlanningResult> {
                match self {
                    $( Self::$variant(inner) => inner.init().await, )*
                }
            }

            async fn next(&mut self, event: $crate::TaskResult) -> anyhow::Result<$crate::planner::PlanningResult> {
                match self {
                    $( Self::$variant(inner) => inner.next(event).await, )*
                }
            }

            async fn abort(&mut self) {
                match self {
                    $( Self::$variant(inner) => inner.abort().await, )*
                }
            }
            fn get_context(&self) -> $crate::Context {
                match self {
                    $( Self::$variant(inner) => inner.get_context(), )*
                }
            }
        }
    };
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident < $($gen:ident),+ $(,)? > {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident($inner:ty)
            ),* $(,)?
        }
        $(where $($wh:tt)+)?
    ) => {
        $(#[$meta])*
        $vis enum $name < $($gen),+ >
        $(where $($wh)+)?
        {
            $(
                $(#[$variant_meta])*
                $variant($inner),
            )*
        }

        #[async_trait::async_trait]
        impl < $($gen: Send + Sync + 'static),+ > $crate::planner::Planning for $name < $($gen),+ >
        $(where $($wh)+)?
        {
            fn id(&self) -> String {
                match self {
                    $( Self::$variant(inner) => inner.id(), )*
                }
            }

            async fn init(&mut self) -> anyhow::Result<$crate::planner::PlanningResult> {
                match self {
                    $( Self::$variant(inner) => inner.init().await, )*
                }
            }

            async fn next(&mut self, event: $crate::TaskResult) -> anyhow::Result<$crate::planner::PlanningResult> {
                match self {
                    $( Self::$variant(inner) => inner.next(event).await, )*
                }
            }

            async fn abort(&mut self) {
                match self {
                    $( Self::$variant(inner) => inner.abort().await, )*
                }
            }
            fn get_context(&self) -> $crate::Context {
                match self {
                    $( Self::$variant(inner) => inner.get_context(), )*
                }
            }
        }
    };
}

#[derive(Debug)]
pub struct NonePlan;
#[async_trait::async_trait]
impl Planning for NonePlan {
    fn id(&self) -> String {
        "".to_string()
    }
    async fn init(&mut self) -> anyhow::Result<PlanningResult> {
        Ok(PlanningResult::End(None))
    }
    async fn next(&mut self, _event: TaskResult) -> anyhow::Result<PlanningResult> {
        Ok(PlanningResult::End(None))
    }
    fn get_context(&self) -> Context {
        Context::default()
    }
}

#[derive(Debug)]
pub struct EndPlanTaskArgs {
    pub plan_id: String,
    pub agent_id: String,
    pub reason: String,
}
impl EndPlanTaskArgs {
    pub fn new(plan_id: String, agent_id: String, reason: String) -> Self {
        Self {
            plan_id,
            agent_id,
            reason,
        }
    }
}
