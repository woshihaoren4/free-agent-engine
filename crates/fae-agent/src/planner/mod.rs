mod plan_to_agent;

use crate::{Env, Event, Task, TaskResult};

/// 构建规划
#[async_trait::async_trait]
pub trait AgentPlanningExt<T: Planning +Send+'static>:Sync {
    fn id(&self) -> String;
    async fn generate_plan(&self, env: Env, event: Event) -> anyhow::Result<T>;
    async fn exit(&self) -> anyhow::Result<()>;
}

#[derive(Debug,Default)]
pub enum PlanningItem{
    #[default]
    Start,
    Result(TaskResult),
}

#[derive(Debug)]
pub enum PlanningResult{
    /// 任务完成,none:不告诉agent，some：告诉agent任务完成结果
    End(Option<TaskResult>),
    Tasks(Vec<Task>),
}

/// 智能体规划 trait，定义智能体的规划逻辑
#[async_trait::async_trait]
pub trait Planning:Sync {
    fn id(&self) -> String;
    /// 执行信息，辅助debug
    async fn show(&self)-> String{
        format!("[{}] running...", self.id())
    }
    async fn start(&mut self) -> anyhow::Result<PlanningResult>;
    /// 下一步规划
    async fn next(&mut self, event: TaskResult) -> anyhow::Result<PlanningResult>;
    /// 强制终止
    async fn abort(&mut self) -> anyhow::Result<()>{
        Ok(())
    }
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

            async fn start(&mut self) -> anyhow::Result<$crate::planner::PlanningResult> {
                match self {
                    $( Self::$variant(inner) => inner.start().await, )*
                }
            }

            async fn next(&mut self, event: $crate::TaskResult) -> anyhow::Result<$crate::planner::PlanningResult> {
                match self {
                    $( Self::$variant(inner) => inner.next(event).await, )*
                }
            }

            async fn abort(&mut self) -> anyhow::Result<()> {
                match self {
                    $( Self::$variant(inner) => inner.abort().await, )*
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
    ) => {
        $(#[$meta])*
        $vis enum $name < $($gen),+ > {
            $(
                $(#[$variant_meta])*
                $variant($inner),
            )*
        }

        #[async_trait::async_trait]
        impl < $($gen: Send + Sync + 'static),+ > $crate::planner::Planning for $name < $($gen),+ > {
            fn id(&self) -> String {
                match self {
                    $( Self::$variant(inner) => inner.id(), )*
                }
            }

            async fn start(&mut self) -> anyhow::Result<$crate::planner::PlanningResult> {
                match self {
                    $( Self::$variant(inner) => inner.start().await, )*
                }
            }

            async fn next(&mut self, event: $crate::TaskResult) -> anyhow::Result<$crate::planner::PlanningResult> {
                match self {
                    $( Self::$variant(inner) => inner.next(event).await, )*
                }
            }

            async fn abort(&mut self) -> anyhow::Result<()> {
                match self {
                    $( Self::$variant(inner) => inner.abort().await, )*
                }
            }
        }
    };
}

#[derive(Debug)]
pub struct EndPlanTaskArgs{
    pub plan_id: String,
    pub agent_id: String,
}
impl EndPlanTaskArgs {
    pub fn new(plan_id: String, agent_id: String) -> Self {
        Self {
            plan_id,
            agent_id,
        }
    }
}