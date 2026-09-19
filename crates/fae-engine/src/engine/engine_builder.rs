use super::Engine;
use crate::engine_rt::{EngineRuntime, PlanRuntime};
use fae_agent::hook::plan::PlanHookBuilder;
use fae_agent::{
    PlanBuilder, PlanBuilderWithEnv, PlanBuilderWithEnvWrapper, Runtime, RuntimeSelectExec,
    TaskType, hook::runtime::RuntimeHookBuilder,
};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

pub struct EngineBuilder {
    plan_builders: HashMap<String, Box<dyn PlanBuilder>>,
    plan_hooks: Vec<Box<dyn PlanHookBuilder>>,
    runtimes: EngineRuntime,
}

impl Debug for EngineBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineBuilder")
            .field("plan_builders", &self.plan_builders)
            .field("plan_hook_count", &self.plan_hooks.len())
            .field("runtimes", &self.runtimes)
            .finish()
    }
}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self {
            plan_builders: HashMap::new(),
            plan_hooks: Vec::new(),
            runtimes: EngineRuntime::new(),
        }
    }
}

impl EngineBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn runtimes(&self) -> &EngineRuntime {
        &self.runtimes
    }

    pub fn runtimes_mut(&mut self) -> &mut EngineRuntime {
        &mut self.runtimes
    }

    pub fn plan_builders(&self) -> &HashMap<String, Box<dyn PlanBuilder>> {
        &self.plan_builders
    }

    pub fn plan_hooks(&self) -> &[Box<dyn PlanHookBuilder>] {
        &self.plan_hooks
    }

    pub fn plan_builder(&self, ty: &str) -> Option<&dyn PlanBuilder> {
        self.plan_builders.get(ty).map(|builder| builder.as_ref())
    }

    pub fn contains_plan_builder(&self, ty: &str) -> bool {
        self.plan_builders.contains_key(ty)
    }

    pub fn add_raw_plan_builder(
        &mut self,
        builder: Box<dyn PlanBuilder>,
    ) -> Option<Box<dyn PlanBuilder>> {
        let ty = builder.ty();
        self.plan_builders.insert(ty, builder)
    }

    pub fn add_plan_builder_with_env_box<ENV>(
        &mut self,
        builder: Box<dyn PlanBuilderWithEnv<ENV>>,
    ) -> Option<Box<dyn PlanBuilder>>
    where
        ENV: Debug + Send + Sync + 'static,
    {
        self.add_raw_plan_builder(Box::new(PlanBuilderWithEnvWrapper::new(builder)))
    }

    pub fn add_plan_builder<T, ENV>(&mut self, builder: T) -> Option<Box<dyn PlanBuilder>>
    where
        ENV: Debug + Send + Sync + 'static,
        T: PlanBuilderWithEnv<ENV>,
    {
        self.add_plan_builder_with_env_box(Box::new(builder))
    }

    pub fn remove_plan_builder(&mut self, ty: &str) -> Option<Box<dyn PlanBuilder>> {
        self.plan_builders.remove(ty)
    }

    pub fn add_plan_hook<H>(&mut self, hook: H)
    where
        H: PlanHookBuilder,
    {
        self.plan_hooks.push(Box::new(hook));
    }

    pub fn add_plan_hook_box(&mut self, hook: Box<dyn PlanHookBuilder>) {
        self.plan_hooks.push(hook);
    }

    pub fn remove_plan_hooks(&mut self) -> Vec<Box<dyn PlanHookBuilder>> {
        std::mem::take(&mut self.plan_hooks)
    }

    pub fn add_raw_runtime(&mut self, rt: Box<dyn Runtime>) -> Option<Arc<dyn Runtime>> {
        self.runtimes.add_raw_runtime(rt)
    }

    pub fn add_runtime_with_tys(
        &mut self,
        rt: Box<dyn Runtime>,
        tys: impl IntoIterator<Item = TaskType>,
    ) -> Option<Arc<dyn Runtime>> {
        self.runtimes.add_raw_runtime_with_tys(rt, tys)
    }

    pub fn add_runtime_arc<Req, Resp, Cond, Info>(
        &mut self,
        rt: Arc<dyn RuntimeSelectExec<Req, Resp, Cond, Info>>,
    ) -> Option<Arc<dyn Runtime>>
    where
        Req: Debug + Send + 'static,
        Resp: Debug + Send + 'static,
        Cond: Debug + Send + 'static,
        Info: Debug + Send + 'static,
    {
        self.runtimes.add_runtime(rt)
    }

    pub fn add_runtime<Req, Resp, Cond, Info, R>(&mut self, rt: R) -> Option<Arc<dyn Runtime>>
    where
        Req: Debug + Send + 'static,
        Resp: Debug + Send + 'static,
        Cond: Debug + Send + 'static,
        Info: Debug + Send + 'static,
        R: RuntimeSelectExec<Req, Resp, Cond, Info>,
    {
        self.add_runtime_arc(Arc::new(rt))
    }

    pub fn remove_runtime(&mut self, id: &str) -> Option<Arc<dyn Runtime>> {
        self.runtimes.remove_runtime(id)
    }

    pub fn add_runtime_hook<H>(&mut self, ty: TaskType, hook: H)
    where
        H: RuntimeHookBuilder,
    {
        self.runtimes.add_runtime_hook(ty, Box::new(hook));
    }

    pub fn add_runtime_hook_box(&mut self, ty: TaskType, hook: Box<dyn RuntimeHookBuilder>) {
        self.runtimes.add_runtime_hook(ty, hook);
    }

    pub fn remove_runtime_hooks(
        &mut self,
        ty: &TaskType,
    ) -> Option<Vec<Box<dyn RuntimeHookBuilder>>> {
        self.runtimes.remove_runtime_hooks(ty)
    }

    pub async fn build(mut self) -> Engine {
        if !self.runtimes.contains_runtime(PlanRuntime::ID) {
            self.add_runtime(PlanRuntime::new());
        }
        let rt = self.runtimes.build().await;
        Engine::with_plan_hooks(self.plan_builders, self.plan_hooks, rt)
    }
}
