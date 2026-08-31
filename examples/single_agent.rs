use std::{
    collections::HashMap,
    io::{self, Write},
};

use fae_agent::{
    Session, SingleAgentEnv, SingleAgentEvent, SingleAgentEventData, SingleAgentInfo,
    SingleAgentModelConfig,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = std::env::var("FAE_DEFAULT_MODEL")
        .map_err(|_| anyhow::anyhow!("set FAE_DEFAULT_MODEL before running this example"))?;
    let engine = fae_engine::Engine::default().await;

    println!("Enter a message. Use /exit or /quit to stop.");
    let Some(first_input) = read_user_input()? else {
        engine.exit().await?;
        return Ok(());
    };

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
            max_completion_tokens: Some(4_096),
            temperature: Some(1.0f32),
            max_tool_iterations: 8,
        },
        first_input,
        vec![fae_engine::READ_FILE.to_string()],
    );

    let first_turn = engine.launch(env).await?;
    print_turn(&session).await?;
    first_turn.result::<()>().await?;

    while let Some(input) = read_user_input()? {
        session.call(input).await?;
        print_turn(&session).await?;
    }

    engine.exit().await?;
    Ok(())
}

fn read_user_input() -> anyhow::Result<Option<String>> {
    loop {
        print!("user> ");
        io::stdout().flush()?;

        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            println!();
            return Ok(None);
        }

        let input = input.trim();
        if matches!(input, "/exit" | "/quit") {
            return Ok(None);
        }
        if !input.is_empty() {
            return Ok(Some(input.to_string()));
        }
    }
}

async fn print_turn(session: &impl Session<String, SingleAgentEvent>) -> anyhow::Result<()> {
    let mut streaming = None;

    while let Some(event) = session.answer().await? {
        let turn_id = event.turn_id;
        let source = event.source;
        match event.data {
            SingleAgentEventData::TurnStarted { input } => {
                println!("\n== Turn {turn_id} | {source} ==\nuser> {input}");
            }
            SingleAgentEventData::HistoryLoaded { messages } => {
                println!("history> loaded {} message(s)", messages.len());
            }
            SingleAgentEventData::UserInput { content } => {
                finish_stream(&mut streaming);
                println!("user> {content}");
            }
            SingleAgentEventData::ModelReasoning { content } => {
                if streaming != Some("reasoning") {
                    finish_stream(&mut streaming);
                    print!("reasoning> ");
                    streaming = Some("reasoning");
                }
                print!("{content}");
                io::stdout().flush()?;
            }
            SingleAgentEventData::ModelOutput { content } => {
                if streaming != Some("assistant") {
                    finish_stream(&mut streaming);
                    print!("assistant> ");
                    streaming = Some("assistant");
                }
                print!("{content}");
                io::stdout().flush()?;
            }
            SingleAgentEventData::ToolCall { arguments, .. } => {
                finish_stream(&mut streaming);
                println!("tool call> {source}\n{}", pretty_json(&arguments));
            }
            SingleAgentEventData::ToolOutput {
                output, completed, ..
            } => {
                finish_stream(&mut streaming);
                let status = if completed { "completed" } else { "streaming" };
                println!("tool result> {source} [{status}]\n{}", pretty_json(&output));
            }
            SingleAgentEventData::Completed { .. } => {
                finish_stream(&mut streaming);
                println!("== Turn {turn_id} completed ==");
                break;
            }
            SingleAgentEventData::Failed { error } => {
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
