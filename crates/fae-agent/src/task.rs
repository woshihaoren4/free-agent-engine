use std::fmt::Debug;
use serde_json::Value;

#[async_trait::async_trait]
pub trait TaskRequest:Debug+Sync{
    async fn get(&self) -> Option<Value>;
}

#[derive(Debug)]
pub struct Task{
    pub id: String,
    pub req: Box<dyn TaskRequest+Send+'static>,
}

#[async_trait::async_trait]
pub trait TaskResponse:Debug+Sync{
    async fn get(&self) -> Option<Value>;
}

#[derive(Debug)]
pub struct TaskResult{
    pub id: String,
    pub result: Box<dyn TaskResponse+Send+'static>,
}