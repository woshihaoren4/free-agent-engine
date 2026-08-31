use std::{
    collections::HashMap,
    io::{self, Write},
};

use fae_agent::{
    Session, SingleAgentEnv, SingleAgentEvent, SingleAgentInfo, SingleAgentModelConfig,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = std::env::var("FAE_DEFAULT_MODEL")
        .map_err(|_| anyhow::anyhow!("set FAE_DEFAULT_MODEL before running this example"))?;
    let engine = fae_engine::Engine::default().await;

    let (env, session) = SingleAgentEnv::new(
        SingleAgentInfo {
            name: "workspace-assistant".to_string(),
            user_id: "example-user".to_string(),
            session_id: "single-agent-example".to_string(),
            metadata: HashMap::new(),
        },
        "Answer concisely. Use an available tool when it is needed.",
        SingleAgentModelConfig {
            model,
            context_size: 32_000,
            history_turns: 10,
            max_completion_tokens: Some(1_024),
            temperature: Some(0.2),
            max_tool_iterations: 8,
        },
        "读取Cargo.toml文件，统计其中的workspace成员数量",
        vec![fae_engine::READ_FILE.to_string()],
    );

    let first_turn = engine.launch(env).await?;
    print_turn(&session).await?;
    first_turn.result::<()>().await?;

    session.call("上一次检查的文件路径是？".to_string()).await?;
    print_turn(&session).await?;

    engine.exit().await?;
    Ok(())
}

async fn print_turn(session: &impl Session<String, SingleAgentEvent>) -> anyhow::Result<()> {
    let mut streaming = None;

    while let Some(event) = session.answer().await? {
        match event {
            SingleAgentEvent::TurnStarted {
                turn_id,
                name,
                input,
            } => {
                println!("\n== Turn {turn_id} | {name} ==\nuser> {input}");
            }
            SingleAgentEvent::HistoryLoaded { messages, .. } => {
                println!("history> loaded {} message(s)", messages.len());
            }
            SingleAgentEvent::UserInput { content, .. } => {
                finish_stream(&mut streaming);
                println!("user> {content}");
            }
            SingleAgentEvent::ModelReasoning { content, .. } => {
                if streaming != Some("reasoning") {
                    finish_stream(&mut streaming);
                    print!("reasoning> ");
                    streaming = Some("reasoning");
                }
                print!("{content}");
                io::stdout().flush()?;
            }
            SingleAgentEvent::ModelOutput { content, .. } => {
                if streaming != Some("assistant") {
                    finish_stream(&mut streaming);
                    print!("assistant> ");
                    streaming = Some("assistant");
                }
                print!("{content}");
                io::stdout().flush()?;
            }
            SingleAgentEvent::ToolCall {
                name, arguments, ..
            } => {
                finish_stream(&mut streaming);
                println!("tool call> {name}\n{}", pretty_json(&arguments));
            }
            SingleAgentEvent::ToolOutput {
                name,
                output,
                completed,
                ..
            } => {
                finish_stream(&mut streaming);
                let status = if completed { "completed" } else { "streaming" };
                println!("tool result> {name} [{status}]\n{}", pretty_json(&output));
            }
            SingleAgentEvent::Completed { turn_id, .. } => {
                finish_stream(&mut streaming);
                println!("== Turn {turn_id} completed ==");
                break;
            }
            SingleAgentEvent::Failed { turn_id, error, .. } => {
                finish_stream(&mut streaming);
                eprintln!("== Turn {turn_id} failed ==\n{error}");
                break;
            }
        }
    }
    Ok(())
}

fn finish_stream(streaming: &mut Option<&'static str>) {
    if streaming.take().is_some() {
        println!();
    }
}

fn pretty_json(value: &str) -> String {
    serde_json::from_str::<serde_json::Value>(value)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .unwrap_or_else(|_| value.to_string())
}
