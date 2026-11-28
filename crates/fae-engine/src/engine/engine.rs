use crate::{EngineContext, PlanRuntime};
use fae_agent::{
    ContextScope, ContextState, Ctx, Plan, PlanBuilder, RT, hook::plan_hook::PlanHookBuilder,
    to_plan_ty,
};
use std::any::type_name;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

#[derive(Clone)]
pub struct Engine {
    generation: Arc<String>,
    plan_routes: Arc<HashMap<String, String>>,
    plan_builders: Arc<HashMap<String, Box<dyn PlanBuilder>>>,
    plan_hooks: Arc<Vec<Box<dyn PlanHookBuilder>>>,
    rt: RT,
    plugins: Option<Arc<fae_agent::plugin::PluginManager>>,
    plugin_snapshot: Option<Arc<fae_agent::plugin::PluginSnapshot>>,
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
    pub(crate) fn with_generation(mut self, generation: String) -> Self {
        self.generation = Arc::new(generation);
        self
    }

    pub fn new(plan_builders: HashMap<String, Box<dyn PlanBuilder>>, rt: RT) -> Self {
        Self::with_plan_hooks(plan_builders, Vec::new(), rt)
    }

    pub(crate) fn with_plan_hooks(
        plan_builders: HashMap<String, Box<dyn PlanBuilder>>,
        plan_hooks: Vec<Box<dyn PlanHookBuilder>>,
        rt: RT,
    ) -> Self {
        Self {
            generation: Arc::new(wd_tools::uuid::v4()),
            plan_routes: Arc::new(HashMap::new()),
            plan_builders: Arc::new(plan_builders),
            plan_hooks: Arc::new(plan_hooks),
            rt,
            plugins: None,
            plugin_snapshot: None,
        }
    }

    pub fn ctx(&self) -> Ctx {
        Ctx::new(EngineContext::into_arc(self.clone()))
    }

    pub fn with_plugins(mut self, plugins: Arc<fae_agent::plugin::PluginManager>) -> Self {
        if self.plugin_snapshot.is_none() {
            self.plugin_snapshot = Some(plugins.snapshot());
        }
        self.plugins = Some(plugins);
        self
    }
    pub(crate) fn with_plugin_snapshot(
        mut self,
        snapshot: Arc<fae_agent::plugin::PluginSnapshot>,
    ) -> Self {
        self.plugin_snapshot = Some(snapshot);
        self
    }

    pub(crate) fn with_plan_routes(mut self, routes: HashMap<String, String>) -> Self {
        self.plan_routes = Arc::new(routes);
        self
    }

    fn validate_context(&self, ctx: &Ctx) -> anyhow::Result<()> {
        ctx.scope()?;
        anyhow::ensure!(
            ctx.get_engine().generation() == self.generation.as_str(),
            "Context belongs to a different Engine generation"
        );
        anyhow::ensure!(
            !ctx.state().current().is_terminal() && !ctx.cancel_requested(),
            "Context is terminal or cancelled"
        );
        Ok(())
    }

    pub async fn execute_plan<Env: Debug + Send + Sync + 'static>(
        &self,
        ctx: Ctx,
        meta: fae_agent::PlanCallMeta,
        env: Env,
    ) -> anyhow::Result<()> {
        fae_agent::Engine::execute_plan(self, ctx, meta, Box::new(env)).await
    }

    pub async fn execute_runtime<Req: Send + 'static, Resp: Send + 'static>(
        &self,
        ctx: Ctx,
        meta: fae_agent::RuntimeCallMeta,
        req: Req,
    ) -> anyhow::Result<fae_agent::TaskResp<Resp>> {
        let mut response =
            fae_agent::Engine::execute_runtime(self, ctx, meta, Box::new(req)).await?;
        fae_agent::TaskResp::try_from_response(&mut response)
            .ok_or_else(|| anyhow::anyhow!("Runtime response type mismatch"))
    }

    pub fn ctx_with_scope(&self, scope: ContextScope) -> anyhow::Result<Ctx> {
        let execution_id = scope
            .execution
            .as_ref()
            .map(|binding| binding.execution_id.clone());
        let ctx = Ctx::new(EngineContext::into_arc(self.clone()));
        ctx.bind_scope(scope)?;
        if let Some(snapshot) = &self.plugin_snapshot {
            ctx.bind_plugin_execution(Arc::new(fae_agent::plugin::PluginExecutionBinding {
                snapshot: snapshot.clone(),
                root: ctx.downgrade(),
                execution_id: execution_id.unwrap_or_else(|| {
                    fae_agent::ExecutionId::new(wd_tools::uuid::v4()).expect("UUID is a valid ID")
                }),
            }));
        }
        Ok(ctx)
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
        self.launch_context(ctx, env).await
    }

    pub async fn launch_with_scope<Env>(&self, scope: ContextScope, env: Env) -> anyhow::Result<Ctx>
    where
        Env: std::fmt::Debug + Send + Sync + 'static,
    {
        self.launch_context(self.ctx_with_scope(scope)?, env).await
    }

    async fn launch_context<Env>(&self, ctx: Ctx, env: Env) -> anyhow::Result<Ctx>
    where
        Env: std::fmt::Debug + Send + Sync + 'static,
    {
        let ty = to_plan_ty::<Env>();
        let plan = self.call(ctx.clone(), ty, env).await?;
        let execution_ctx = ctx.clone();

        tokio::spawn(async move {
            let result = Self::run_plan(plan, execution_ctx.clone()).await;
            match result {
                Ok(()) => {
                    if !execution_ctx.state().current().is_terminal() {
                        let _ = execution_ctx.set_state(ContextState::succeeded(()));
                    }
                }
                Err(error) => {
                    let _ = execution_ctx.set_state(ContextState::failed(error.to_string()));
                }
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
            Ok(()) => {
                if !ctx.state().current().is_terminal() {
                    let _ = ctx.set_state(ContextState::succeeded(()));
                }
            }
            Err(error) => {
                let _ = ctx.set_state(ContextState::failed(error.to_string()));
            }
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

        self.build_with_builder(builder, ctx, Box::new(env)).await
    }

    async fn build_with_builder(
        &self,
        builder: &dyn PlanBuilder,
        ctx: Ctx,
        env: fae_agent::AnyType,
    ) -> anyhow::Result<Box<dyn Plan>> {
        let execution_ctx = ctx.clone();
        let mut plan = match builder.build(self.rt(), ctx, env).await {
            Ok(plan) => plan,
            Err(error) => {
                execution_ctx.discard_output();
                let _ = execution_ctx.cleanup().await;
                let cancellation = execution_ctx
                    .cancellation_signal()
                    .and_then(|signal| signal.borrow().clone());
                let state = match cancellation {
                    Some(reason) => ContextState::cancelled(reason),
                    None => ContextState::failed(error.to_string()),
                };
                let _ = execution_ctx.set_state(state);
                return Err(error);
            }
        };
        // Compatibility for generic top-level builders with no user/session fields.
        if execution_ctx.scope().is_err() {
            execution_ctx.bind_scope(ContextScope::root(fae_agent::DEFAULT_USER_ID))?;
        }
        if let Err(error) = execution_ctx.set_state(ContextState::Running) {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                plan.abort(-1, error.to_string()),
            )
            .await;
            execution_ctx.discard_output();
            let _ = execution_ctx.cleanup().await;
            return Err(error);
        }
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
    fn generation(&self) -> &str {
        self.generation.as_str()
    }
    fn create_context(&self, scope: ContextScope) -> anyhow::Result<Ctx> {
        self.ctx_with_scope(scope)
    }
    fn plans(&self) -> Vec<fae_agent::PlanDescriptor> {
        let mut plans = self
            .plan_routes
            .iter()
            .map(|(id, ty)| fae_agent::PlanDescriptor {
                id: id.clone(),
                env_type: ty.clone(),
            })
            .collect::<Vec<_>>();
        plans.sort_by(|a, b| a.id.cmp(&b.id));
        plans
    }
    async fn build_plan(
        &self,
        ctx: Ctx,
        meta: fae_agent::PlanCallMeta,
        env: fae_agent::AnyType,
    ) -> anyhow::Result<Box<dyn Plan>> {
        self.validate_context(&ctx)?;
        let ty = self
            .plan_routes
            .get(&meta.plan_id)
            .ok_or_else(|| anyhow::anyhow!("plan_not_found: {}", meta.plan_id))?;
        let builder = self
            .plan_builder(ty)
            .ok_or_else(|| anyhow::anyhow!("Plan route builder is missing: {ty}"))?;
        self.build_with_builder(builder, ctx, env).await
    }
    async fn fork_plan(
        &self,
        ctx: &Ctx,
        meta: fae_agent::PlanCallMeta,
        mut env: fae_agent::AnyType,
    ) -> anyhow::Result<(Ctx, Box<dyn Plan>)> {
        self.validate_context(ctx)?;
        let ty = self
            .plan_routes
            .get(&meta.plan_id)
            .ok_or_else(|| anyhow::anyhow!("plan_not_found: {}", meta.plan_id))?;
        let builder = self
            .plan_builder(ty)
            .ok_or_else(|| anyhow::anyhow!("Plan route builder is missing: {ty}"))?;
        let child = ctx.fork(builder.prepare_child_spec(ctx, &mut env).await?)?;
        let plan = self.build_with_builder(builder, child.clone(), env).await?;
        Ok((child, plan))
    }
    async fn execute_plan(
        &self,
        ctx: Ctx,
        meta: fae_agent::PlanCallMeta,
        env: fae_agent::AnyType,
    ) -> anyhow::Result<()> {
        let plan = fae_agent::Engine::build_plan(self, ctx.clone(), meta, env).await?;
        Self::run_plan(plan, ctx).await
    }
    async fn execute_runtime(
        &self,
        ctx: Ctx,
        meta: fae_agent::RuntimeCallMeta,
        req: fae_agent::AnyType,
    ) -> anyhow::Result<fae_agent::TaskResponse> {
        self.validate_context(&ctx)?;
        let mut task = fae_agent::TaskRequest::from_parts(
            ctx,
            fae_agent::TaskMeta {
                id: wd_tools::uuid::v4(),
                executor: meta.runtime_id,
                ty: meta.ty,
                ..Default::default()
            },
            req,
        );
        Ok(fae_agent::Runtime::exec(&*self.rt, &mut task).await?)
    }
    async fn select_on(
        &self,
        ctx: &Ctx,
        meta: fae_agent::RuntimeCallMeta,
        mut cond: fae_agent::AnyType,
    ) -> anyhow::Result<fae_agent::AnyType> {
        self.validate_context(ctx)?;
        Ok(fae_agent::Runtime::select_on(&*self.rt, ctx, &meta, &mut cond).await?)
    }
    fn context(&self) -> Ctx {
        self.ctx()
    }
    async fn fork_call(
        &self,
        ctx: &Ctx,
        ty: String,
        mut env: fae_agent::AnyType,
    ) -> anyhow::Result<(Ctx, Box<dyn Plan>)> {
        let builder = self
            .plan_builder(&ty)
            .ok_or_else(|| anyhow::anyhow!("plan builder is not registered for `{ty}`"))?;
        let child = ctx.fork(builder.prepare_child_spec(ctx, &mut env).await?)?;
        let plan = self.build_with_builder(builder, child.clone(), env).await?;
        Ok((child, plan))
    }
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

        self.build_with_builder(builder, ctx, env).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EngineBuilder;
    use fae_agent::{PlanBuilderWithEnv, PlanNext, TaskResponse, hook::plan_hook::PlanHookBuilder};
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

        assert!(ctx.state().current().is_terminal());
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
