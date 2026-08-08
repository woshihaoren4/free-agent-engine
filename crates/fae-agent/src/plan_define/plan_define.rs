use std::fmt::{Debug, Formatter};
use crate::{common, RT, Event, Task, TaskResult};

#[derive(Debug)]
pub enum PlanNext{
    Tasks(Vec<Task>),
    End,
}

#[async_trait::async_trait]
pub trait Plan: Debug + Send + Sync + 'static {
    fn id(&self)->&str;
    async fn init(&mut self)->anyhow::Result<PlanNext>;
    async fn next(&mut self,task_result:TaskResult) -> anyhow::Result<PlanNext>;
    async fn abort(&mut self,code:i32,error: String);
}
#[async_trait::async_trait]
pub trait PlanWithEnv<ENV>: Debug + Send + Sync + 'static{
    fn id(&self)->&str;
    async fn init(&mut self, env : &mut ENV) ->anyhow::Result<PlanNext>;
    async fn next(&mut self, env : &mut ENV, task_result:TaskResult) -> anyhow::Result<PlanNext>;
    async fn abort(&mut self, env : &mut ENV, code:i32, error: String);
}

#[derive(Debug)]
pub struct PlanWithEnvWrapper<ENV>{
    pub env: ENV,
    pub plan: Box<dyn PlanWithEnv<ENV>>,
}
impl <ENV: 'static> PlanWithEnvWrapper<ENV> {
    pub fn new(env: ENV, plan: Box<dyn PlanWithEnv<ENV>>) -> Self {
        Self {
            env,
            plan,
        }
    }
}

#[async_trait::async_trait]
impl<ENV:Debug + Send + Sync + 'static> Plan for PlanWithEnvWrapper<ENV>
{
    fn id(&self) -> &str {
        self.plan.id()
    }

    async fn init(&mut self)->anyhow::Result<PlanNext> {
        self.plan.init(&mut self.env).await
    }
    async fn next(&mut self,task_result:TaskResult) -> anyhow::Result<PlanNext> {
        self.plan.next(&mut self.env, task_result).await
    }
    async fn abort(&mut self,code:i32,error: String) {
        self.plan.abort(&mut self.env, code, error).await
    }
}


