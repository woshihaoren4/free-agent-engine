mod plan_to_agent;

use crate::{Env, Event, Task};

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

            async fn next(&mut self, event: $crate::Event) -> anyhow::Result<Vec<$crate::Task>> {
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

            async fn next(&mut self, event: $crate::Event) -> anyhow::Result<Vec<$crate::Task>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Event, Task, TaskType};

    #[derive(Default)]
    struct PlanA {
        count: usize,
    }

    #[async_trait::async_trait]
    impl Planning for PlanA {
        fn id(&self) -> String {
            "PlanA".to_string()
        }

        async fn next(&mut self, _event: Event) -> anyhow::Result<Vec<Task>> {
            self.count += 1;
            if self.count > 2 {
                return Err(crate::Error::PlanOver.into());
            }
            Ok(vec![Task {
                id: format!("A-{}", self.count),
                agent_id: "agent-a".to_string(),
                r#type: TaskType::None,
                exec_channel: "default".to_string(),
                args: None,
            }])
        }

        async fn abort(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct PlanB {
        count: usize,
    }

    #[async_trait::async_trait]
    impl Planning for PlanB {
        fn id(&self) -> String {
            "PlanB".to_string()
        }

        async fn next(&mut self, _event: Event) -> anyhow::Result<Vec<Task>> {
            self.count += 1;
            if self.count > 1 {
                return Err(crate::Error::PlanOver.into());
            }
            Ok(vec![Task {
                id: format!("B-{}", self.count),
                agent_id: "agent-b".to_string(),
                r#type: TaskType::None,
                exec_channel: "default".to_string(),
                args: None,
            }])
        }

        async fn abort(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    define_planning_group! {
        pub enum MyPlanGroup {
            A(PlanA),
            B(PlanB),
        }
    }

    #[tokio::test]
    async fn test_planning_group_aggregation() {
        let mut plan_a = MyPlanGroup::A(PlanA::default());
        let mut plan_b = MyPlanGroup::B(PlanB::default());

        assert_eq!(plan_a.id(), "PlanA");
        assert_eq!(plan_b.id(), "PlanB");

        // 测试 Plan A
        let tasks_a1 = plan_a.next(Event::default()).await.unwrap();
        assert_eq!(tasks_a1.len(), 1);
        assert_eq!(tasks_a1[0].id, "A-1");

        let tasks_a2 = plan_a.next(Event::default()).await.unwrap();
        assert_eq!(tasks_a2.len(), 1);
        assert_eq!(tasks_a2[0].id, "A-2");

        let over_a = plan_a.next(Event::default()).await;
        assert!(over_a.is_err()); // 应该返回 PlanOver 错误

        // 测试 Plan B
        let tasks_b1 = plan_b.next(Event::default()).await.unwrap();
        assert_eq!(tasks_b1.len(), 1);
        assert_eq!(tasks_b1[0].id, "B-1");

        let over_b = plan_b.next(Event::default()).await;
        assert!(over_b.is_err()); // 应该返回 PlanOver 错误

        // 测试 abort
        plan_a.abort().await.unwrap();
        plan_b.abort().await.unwrap();
    }

    #[derive(Default)]
    struct PlanC<T> {
        count: usize,
        _marker: std::marker::PhantomData<T>,
    }

    #[async_trait::async_trait]
    impl<T: Send + Sync + 'static> Planning for PlanC<T> {
        fn id(&self) -> String {
            "PlanC".to_string()
        }

        async fn next(&mut self, _event: Event) -> anyhow::Result<Vec<Task>> {
            Err(crate::Error::PlanOver.into())
        }

        async fn abort(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    define_planning_group! {
        pub enum MyGenericPlanGroup<T> {
            C(PlanC<T>),
            B(PlanB),
        }
    }

    #[tokio::test]
    async fn test_generic_planning_group() {
        let mut plan = MyGenericPlanGroup::<String>::C(PlanC::default());
        assert_eq!(plan.id(), "PlanC");
        let over = plan.next(Event::default()).await;
        assert!(over.is_err());
    }
}