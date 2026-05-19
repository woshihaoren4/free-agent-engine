mod engine;
mod engine_builder;
mod executors;
mod runtime;
mod workspace;
mod workspace_builder;

pub use engine::*;
pub use executors::*;
pub use runtime::*;
pub use workspace::*;
pub use workspace_builder::*;



#[cfg(test)]
mod tests {
    use fae_agent::{TaskType, ThingSelect};
    use super::*;

    #[tokio::test]
    async fn test_engine() {
        let mut engine = AgentsEngine::default().await;
        let ws = engine.build_workspace("main",(),|x|{}).await;
        let executor_info = ws.get_env().query(ThingSelect::Executor(TaskType::Model,EXECUTOR_OPENAI_API_CHANNEL.into())).await.expect("Failed to get executor info");
        println!("{:?}", executor_info);
    }
}
