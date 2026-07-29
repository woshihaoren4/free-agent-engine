use std::fmt::Debug;
use crate::{Ctx, Env, Task, TaskResult};

#[derive(Debug)]
pub enum PlanNext{
    Tasks(Vec<Task>),
    End,
}

#[async_trait::async_trait]
pub trait Plan: Debug + Send + Sync + 'static {
    async fn init(&mut self,env:Env,ctx:Ctx)->anyhow::Result<()>;
    async fn next(&mut self,env:Env,ctx:Ctx,task_result:TaskResult) -> anyhow::Result<PlanNext>;
    async fn abort(&mut self,env:Env,ctx:Ctx, error: String)->anyhow::Result<()>;
}