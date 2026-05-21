use fae_agent::{EXECUTOR_OPENAI_API_CHANNEL, TaskType, ThingSelect};
use fae_engine::AgentsEngine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Initializing AgentsEngine...");
    let mut engine = AgentsEngine::default().await;

    println!("Building workspace 'main'...");
    let ws = engine.build_workspace("main", |_x| {}).await;

    println!("Querying executor info...");
    let executor_info = ws
        .get_env()
        .query(ThingSelect::Executor(
            TaskType::Model,
            EXECUTOR_OPENAI_API_CHANNEL.into(),
        ))
        .await
        .expect("Failed to get executor info");

    println!("Executor Info: {:?}", executor_info);

    Ok(())
}
