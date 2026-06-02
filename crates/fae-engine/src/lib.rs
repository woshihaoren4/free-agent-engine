mod engine;
mod engine_builder;
mod executors;
mod runtime;
mod workspace;
mod tools;

pub use engine::*;
pub use executors::*;
pub use runtime::*;
pub use workspace::*;

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::{EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL, TaskType, ThingSelect};

    #[tokio::test]
    async fn test_engine() {
        let mut engine = AgentsEngine::default().await;
        let ws = engine.build_workspace("main", |x| {}).await;
        let executor_info = ws
            .get_env()
            .query(ThingSelect::Executor(
                TaskType::Model,
                EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL.into(),
            ).into())
            .await
            .expect("Failed to get executor info");
        println!("{:?}", executor_info);
    }
}
