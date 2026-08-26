use crate::engine_rt::EngineRuntime;
use fae_agent::{
    PlanBuilder, PlanBuilderWithEnv, PlanBuilderWithEnvWrapper, Runtime, RuntimeSelectExec,
    TaskType,
};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

#[derive(Debug)]
pub struct EngineBuilder {
    plan_builders: HashMap<String, Box<dyn PlanBuilder>>,
    runtimes: EngineRuntime,
}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self {
            plan_builders: HashMap::new(),
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

    pub fn add_plan_builder_with_env<ENV>(
        &mut self,
        builder: Box<dyn PlanBuilderWithEnv<ENV>>,
    ) -> Option<Box<dyn PlanBuilder>>
    where
        ENV: Debug + Send + Sync + 'static,
    {
        self.add_raw_plan_builder(Box::new(PlanBuilderWithEnvWrapper::new(builder)))
    }

    pub fn remove_plan_builder(&mut self, ty: &str) -> Option<Box<dyn PlanBuilder>> {
        self.plan_builders.remove(ty)
    }

    pub fn add_raw_runtime(&mut self, rt: Box<dyn Runtime>) -> Option<Box<dyn Runtime>> {
        self.runtimes.add_raw_runtime(rt)
    }

    pub fn add_runtime_with_tys(
        &mut self,
        rt: Box<dyn Runtime>,
        tys: impl IntoIterator<Item = TaskType>,
    ) -> Option<Box<dyn Runtime>> {
        self.runtimes.add_raw_runtime_with_tys(rt, tys)
    }

    pub fn add_runtime<Req, Resp, Cond, Info>(
        &mut self,
        rt: Arc<dyn RuntimeSelectExec<Req, Resp, Cond, Info>>,
    ) -> Option<Box<dyn Runtime>>
    where
        Req: Debug + Send + 'static,
        Resp: Debug + Send + 'static,
        Cond: Debug + Send + 'static,
        Info: Debug + Send + 'static,
    {
        self.runtimes.add_runtime(rt)
    }

    pub fn remove_runtime(&mut self, id: &str) -> Option<Box<dyn Runtime>> {
        self.runtimes.remove_runtime(id)
    }
}
