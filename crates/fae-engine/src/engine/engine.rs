use crate::{EngineContext, PlanRuntime};
use fae_agent::{Ctx, Plan, PlanBuilder, RT, hook::plan::PlanHookBuilder, to_plan_ty};
use std::any::type_name;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

#[derive(Clone)]
pub struct Engine {
    plan_builders: Arc<HashMap<String, Box<dyn PlanBuilder>>>,
    plan_hooks: Arc<Vec<Box<dyn PlanHookBuilder>>>,
    rt: RT,
}

impl Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("plan_builders", &self.plan_builders)
            .field("plan_hook_count", &self.plan_hooks.len())
            .field("rt", &self.rt)
            .finish()
    }
}

impl Engine {
    pub fn new(plan_builders: HashMap<String, Box<dyn PlanBuilder>>, rt: RT) -> Self {
        Self::with_plan_hooks(plan_builders, Vec::new(), rt)
    }

    pub(crate) fn with_plan_hooks(
        plan_builders: HashMap<String, Box<dyn PlanBuilder>>,
        plan_hooks: Vec<Box<dyn PlanHookBuilder>>,
        rt: RT,
    ) -> Self {
        Self {
            plan_builders: Arc::new(plan_builders),
            plan_hooks: Arc::new(plan_hooks),
            rt,
        }
    }

    pub fn ctx(&self) -> Ctx {
        Ctx::new(EngineContext::into_arc(self.clone()))
    }

    pub fn rt(&self) -> RT {
        self.rt.clone()
    }

    pub fn plan_builders(&self) -> &HashMap<String, Box<dyn PlanBuilder>> {
        self.plan_builders.as_ref()
    }

    pub fn plan_builder(&self, ty: &str) -> Option<&dyn PlanBuilder> {
        self.plan_builders.get(ty).map(|builder| builder.as_ref())
    }

    pub fn plan_hooks(&self) -> &[Box<dyn PlanHookBuilder>] {
        self.plan_hooks.as_ref()
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

        self.build_plan(builder, ctx, Box::new(env)).await
    }

    async fn build_plan(
        &self,
        builder: &dyn PlanBuilder,
        ctx: Ctx,
        env: fae_agent::AnyType,
    ) -> anyhow::Result<Box<dyn Plan>> {
        let mut plan = builder.build(self.rt(), ctx, env).await?;
        for hook in self.plan_hooks.iter() {
            plan = hook.build(plan).await;
        }
        Ok(plan)
    }

    async fn run_plan(plan: Box<dyn Plan>, ctx: Ctx) -> anyhow::Result<()> {
        PlanRuntime::run_plan(plan, ctx).await
    }
}

#[async_trait::async_trait]
impl fae_agent::Engine for Engine {
    fn rt(&self) -> RT {
        self.rt()
    }

    async fn call(
        &self,
        ctx: Ctx,
        ty: String,
        env: fae_agent::AnyType,
    ) -> anyhow::Result<Box<dyn Plan>> {
        let builder = self.plan_builder(&ty).ok_or_else(|| {
            anyhow::anyhow!("plan builder is not registered for environment type `{ty}`")
        })?;

        self.build_plan(builder, ctx, env).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EngineBuilder;
    use fae_agent::{PlanBuilderWithEnv, PlanNext, TaskResponse, hook::plan::PlanHookBuilder};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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

    struct CountingPlanHookBuilder {
        builds: Arc<AtomicUsize>,
        inits: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl PlanHookBuilder for CountingPlanHookBuilder {
        async fn build(&self, plan: Box<dyn Plan>) -> Box<dyn Plan> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            Box::new(CountingPlanHook {
                plan,
                inits: self.inits.clone(),
            })
        }
    }

    #[derive(Debug)]
    struct CountingPlanHook {
        plan: Box<dyn Plan>,
        inits: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Plan for CountingPlanHook {
        fn id(&self) -> &str {
            self.plan.id()
        }

        async fn init(&mut self) -> anyhow::Result<PlanNext> {
            self.inits.fetch_add(1, Ordering::SeqCst);
            self.plan.init().await
        }

        async fn next(&mut self, task_result: TaskResponse) -> anyhow::Result<PlanNext> {
            self.plan.next(task_result).await
        }

        async fn abort(&mut self, code: i32, error: String) {
            self.plan.abort(code, error).await;
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

    #[tokio::test]
    async fn clone_shares_plan_builders_and_context_exposes_engine() {
        let engine = test_engine(Arc::new(AtomicBool::new(false))).await;
        let cloned = engine.clone();
        let ctx = engine.ctx();

        assert!(Arc::ptr_eq(&engine.plan_builders, &cloned.plan_builders));
        assert_eq!(engine.rt().id(), ctx.get_engine().rt().id());

        ctx.get_engine()
            .call(
                ctx.clone(),
                to_plan_ty::<TestEnv>(),
                Box::new(TestEnv { fail: false }),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn builds_plan_hooks_for_every_call() {
        let builds = Arc::new(AtomicUsize::new(0));
        let inits = Arc::new(AtomicUsize::new(0));
        let mut builder = EngineBuilder::new();
        builder.add_plan_builder_with_env_box(Box::new(TestPlanBuilder {
            aborted: Arc::new(AtomicBool::new(false)),
        }));
        builder.add_plan_hook(CountingPlanHookBuilder {
            builds: builds.clone(),
            inits: inits.clone(),
        });
        let engine = builder.build().await;

        let mut typed_plan = engine
            .call(
                engine.ctx(),
                to_plan_ty::<TestEnv>(),
                TestEnv { fail: false },
            )
            .await
            .unwrap();
        typed_plan.init().await.unwrap();

        let mut erased_plan = fae_agent::Engine::call(
            &engine,
            engine.ctx(),
            to_plan_ty::<TestEnv>(),
            Box::new(TestEnv { fail: false }),
        )
        .await
        .unwrap();
        erased_plan.init().await.unwrap();

        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert_eq!(inits.load(Ordering::SeqCst), 2);
    }
}
