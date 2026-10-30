use crate::{Ctx, Plan, RT, common};
use std::fmt::Debug;

#[async_trait::async_trait]
pub trait PlanBuilder: Debug + Send + Sync + 'static {
    fn ty(&self) -> String;
    async fn build(&self, rt: RT, ctx: Ctx, env: common::AnyType) -> anyhow::Result<Box<dyn Plan>>;
    async fn child_spec(
        &self,
        _env: &mut common::AnyType,
    ) -> anyhow::Result<crate::ContextChildSpec> {
        Ok(crate::ContextChildSpec {
            execution: true,
            ..Default::default()
        })
    }
}

#[async_trait::async_trait]
pub trait PlanBuilderWithEnv<ENV: 'static>: Debug + Send + Sync + 'static {
    fn ty(&self) -> String {
        super::to_plan_ty::<ENV>()
    }
    async fn build(&self, rt: RT, ctx: Ctx, env: ENV) -> anyhow::Result<Box<dyn Plan>>;
    async fn child_spec(&self, _env: &ENV) -> anyhow::Result<crate::ContextChildSpec> {
        Ok(crate::ContextChildSpec {
            execution: true,
            ..Default::default()
        })
    }
}

#[derive(Debug)]
pub struct PlanBuilderWithEnvWrapper<ENV> {
    inner: Box<dyn PlanBuilderWithEnv<ENV>>,
}

impl<ENV> PlanBuilderWithEnvWrapper<ENV> {
    pub fn new(inner: Box<dyn PlanBuilderWithEnv<ENV>>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl<ENV> PlanBuilder for PlanBuilderWithEnvWrapper<ENV>
where
    ENV: Debug + Send + Sync + 'static,
{
    fn ty(&self) -> String {
        self.inner.ty()
    }

    async fn build(&self, rt: RT, tx: Ctx, env: common::AnyType) -> anyhow::Result<Box<dyn Plan>> {
        let env = env
            .downcast::<ENV>()
            .expect("[PlanBuilderWithEnvWrapper<ENV>::PlanBuilder.build] env is not ENV");
        self.inner.build(rt, tx, *env).await
    }

    async fn child_spec(
        &self,
        env: &mut common::AnyType,
    ) -> anyhow::Result<crate::ContextChildSpec> {
        let env = env
            .downcast_ref::<ENV>()
            .ok_or_else(|| anyhow::anyhow!("plan environment type mismatch"))?;
        self.inner.child_spec(env).await
    }
}
