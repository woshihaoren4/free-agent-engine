use std::fmt::Debug;
use crate::{Ctx, RT, Event, Plan, common};

#[async_trait::async_trait]
pub trait PlanGenerator: Debug + Send + Sync + 'static {
    fn ty(&self) -> String;
    async fn generate(&self, rt: RT, ctx: Ctx, env: common::AnyType) -> anyhow::Result<Box<dyn Plan>>;
}

#[async_trait::async_trait]
pub trait PlanGeneratorWithEnv<ENV>: Debug + Send + Sync + 'static {
    async fn generate(&self, rt: RT, ctx: Ctx, env: ENV) -> anyhow::Result<Box<dyn Plan>>;
}

#[derive(Debug)]
pub struct PlanGeneratorWithEnvImpl<ENV> {
    inner: Box<dyn PlanGeneratorWithEnv<ENV>>,
}

#[async_trait::async_trait]
impl<ENV> PlanGenerator for PlanGeneratorWithEnvImpl<ENV>
where
    ENV: Debug + Send + Sync + 'static,
{
    fn ty(&self) -> String {
        super::to_plan_ty::<ENV>()
    }

    async fn generate(&self, rt: RT, tx: Ctx, env: common::AnyType) -> anyhow::Result<Box<dyn Plan>> {
        let env = env.downcast::<ENV>().expect("[PlanGeneratorWithEnvImpl<ENV>::PlanGenerator.generate] env is not ENV");
        self.inner.generate(rt, tx, *env).await
    }
}
