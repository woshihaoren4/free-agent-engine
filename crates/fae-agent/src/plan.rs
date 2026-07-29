

#[async_trait::async_trait]
pub trait Plan: Debug + Send + Sync + 'static {
    /// 计划任务
    async fn next(&self, env: Env, ctx: Context) -> anyhow::Result<Vec<Task>>;
}