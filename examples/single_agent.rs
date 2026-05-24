use fae_agent::{AgentConfigData, OpenAIMemoryEntry, Record, SingleAgentSessionConfig};
use fae_engine::AgentsEngine;
use std::io::{self, Write};
use std::pin::Pin;
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Initializing AgentsEngine...");
    let mut engine = AgentsEngine::default().await;

    println!("Building workspace 'test_workspace'...");
    let ws = engine.build_workspace("test_workspace", |_x| {}).await;

    println!("Checking if agent exists...");
    if ws.get_agent("main_agent").await.is_err() {
        println!("Creating SingleAgent...");
        let prompt = "You are a helpful assistant.";
        let config = Box::new(AgentConfigData::default());
        ws.create_agent("main_agent", prompt, config).await?;
    }

    println!("Creating session...");
    let mut session = ws
        .session_call_stream::<_, Record, Record>(
            "main_agent",
            SingleAgentSessionConfig::default().set_id("test_session_id_123"),
        )
        .await?;

    println!("Session started. Type '/exit' to quit.");

    loop {
        print!("You: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input == "/exit" {
            break;
        }

        if input.is_empty() {
            continue;
        }

        let msg = Record::from_user_input(input);
        let stream = session.call_stream(msg).await?;
        let mut stream = Pin::from(stream);

        print!("Agent: ");
        io::stdout().flush()?;
        let mut title = String::new();
        while let Some(record) = stream.next().await {
            let t = record.title();
            if title != t {
                title = t;
                println!("\n---> {} <---", title);
            }
            print!("{}", record.content());
            io::stdout().flush()?;
        }
        println!();
    }

    println!("Session finished.");

    ws.exit().await;
    Ok(())
}
