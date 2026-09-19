use crate::Plan;

#[async_trait::async_trait]
pub trait PlanHookBuilder: Send + Sync + 'static {
    async fn build(&self, plan: Box<dyn Plan>) -> Box<dyn Plan>;
}
