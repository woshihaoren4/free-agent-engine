use std::collections::HashMap;
use fae_agent::{Event, Runtime, Task, TaskResult};

pub struct EngineRuntime{
    rts: HashMap<String,Box<dyn Runtime>>,
}

#[async_trait::async_trait]
impl Runtime for EngineRuntime{
    async fn watch(&self) -> anyhow::Result<Event> {
        todo!()
    }

    async fn select(&self, key: &str) -> anyhow::Result<String> {
        todo!()
    }

    async fn spawn(&self, tasks: Task) -> anyhow::Result<()> {
        todo!()
    }

    async fn exec(&self, task: Task) -> anyhow::Result<TaskResult> {
        todo!()
    }

    async fn kill(&self, task_id: &str) -> anyhow::Result<()> {
        todo!()
    }

    async fn exit(&self) {
        todo!()
    }
}