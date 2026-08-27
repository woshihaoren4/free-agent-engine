use crate::{EngineContext, PlanRuntime};
use fae_agent::{Ctx, Plan, PlanBuilder, RT, to_plan_ty};
use std::any::type_name;
use std::collections::HashMap;

#[derive(Debug)]
pub struct Engine {
    plan_builders: HashMap<String, Box<dyn PlanBuilder>>,
    rt: RT,
}

impl Engine {
    pub fn new(plan_builders: HashMap<String, Box<dyn PlanBuilder>>, rt: RT) -> Self {
        Self { plan_builders, rt }
    }

    pub fn ctx(&self) -> Ctx {
        Ctx::new(EngineContext::into_arc(self.rt()))
    }

    pub fn rt(&self) -> RT {
        self.rt.clone()
    }

    pub fn plan_builders(&self) -> &HashMap<String, Box<dyn PlanBuilder>> {
        &self.plan_builders
    }

    pub fn plan_builder(&self, ty: &str) -> Option<&dyn PlanBuilder> {
        self.plan_builders.get(ty).map(|builder| builder.as_ref())
    }

    pub async fn exit(&self) -> fae_agent::Result<()> {
        self.rt.exit().await
    }

    // launch: 启动一个任务
    pub async fn launch<Env>(&self, env: Env) -> anyhow::Result<Ctx>
    where
        Env: std::fmt::Debug + Send + Sync + 'static,
    {
        let ctx = self.ctx();
        let ty = to_plan_ty::<Env>();
        let plan = self.call(ctx.clone(), ty, env).await?;
        let execution_ctx = ctx.clone();

        tokio::spawn(async move {
            let result = Self::run_plan(plan, execution_ctx.clone()).await;
            match result {
                Ok(()) => execution_ctx.over(Box::new(())),
                Err(error) => execution_ctx.error(error.to_string()),
            }
        });

        Ok(ctx)
    }

    // invoke: 执行一个任务
    pub async fn invoke<Env, Out>(&self, env: Env) -> anyhow::Result<(Ctx, Out)>
    where
        Env: std::fmt::Debug + Send + Sync + 'static,
        Out: Send + 'static,
    {
        let ctx = self.ctx();
        let ty = to_plan_ty::<Env>();
        let plan = self.call(ctx.clone(), ty, env).await?;
        let result = Self::run_plan(plan, ctx.clone()).await;
        match result {
            Ok(()) => ctx.over(Box::new(())),
            Err(error) => ctx.error(error.to_string()),
        }
        let output = ctx.result::<Out>().await?;
        Ok((ctx, output))
    }

    pub async fn call<Env>(&self, ctx: Ctx, ty: String, env: Env) -> anyhow::Result<Box<dyn Plan>>
    where
        Env: std::fmt::Debug + Send + Sync + 'static,
    {
        let builder = self.plan_builder(&ty).ok_or_else(|| {
            anyhow::anyhow!(
                "plan builder is not registered for environment type `{}` ({ty})",
                type_name::<Env>()
            )
        })?;

        builder.build(self.rt(), ctx, Box::new(env)).await
    }

    async fn run_plan(plan: Box<dyn Plan>, ctx: Ctx) -> anyhow::Result<()> {
        PlanRuntime::run_plan(plan, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EngineBuilder;
    use fae_agent::{PlanBuilderWithEnv, PlanNext, TaskResponse};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[derive(Debug)]
    struct TestEnv {
        fail: bool,
    }

    #[derive(Debug)]
    struct TestPlanBuilder {
        aborted: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl PlanBuilderWithEnv<TestEnv> for TestPlanBuilder {
        async fn build(&self, _rt: RT, _ctx: Ctx, env: TestEnv) -> anyhow::Result<Box<dyn Plan>> {
            Ok(Box::new(TestPlan {
                fail: env.fail,
                aborted: self.aborted.clone(),
            }))
        }
    }

    #[derive(Debug)]
    struct TestPlan {
        fail: bool,
        aborted: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Plan for TestPlan {
        fn id(&self) -> &str {
            "test"
        }

        async fn init(&mut self) -> anyhow::Result<PlanNext> {
            if self.fail {
                anyhow::bail!("init failed");
            }
            Ok(PlanNext::End)
        }

        async fn next(&mut self, _task_result: TaskResponse) -> anyhow::Result<PlanNext> {
            Ok(PlanNext::End)
        }

        async fn abort(&mut self, _code: i32, _error: String) {
            self.aborted.store(true, Ordering::SeqCst);
        }
    }

    async fn test_engine(aborted: Arc<AtomicBool>) -> Engine {
        let mut builder = EngineBuilder::new();
        builder.add_plan_builder_with_env_box(Box::new(TestPlanBuilder { aborted }));
        builder.build().await
    }

    #[tokio::test]
    async fn launch_returns_waitable_context() {
        let engine = test_engine(Arc::new(AtomicBool::new(false))).await;

        let ctx = engine.launch(TestEnv { fail: false }).await.unwrap();
        ctx.result::<()>().await.unwrap();

        assert!(ctx.is_completed());
    }
}
