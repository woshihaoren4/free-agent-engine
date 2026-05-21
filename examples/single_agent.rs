use fae_agent::{AgentConfigData, Message, SessionInfo};
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
        .session(
            "main_agent",
            SessionInfo {
                session_id: "test_session_2".to_string(),
                ..Default::default()
            },
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

        let msg = Message::default().set_content(input.to_string());
        let stream = session.call_stream(msg).await?;
        let mut stream = Pin::from(stream);

        print!("Agent: ");
        io::stdout().flush()?;
        while let Some(mut resp) = stream.next().await {
            if let Some(content) = resp.try_into_inner::<String>() {
                print!("{}", content);
                io::stdout().flush()?;
            }
        }
        println!();
    }

    println!("Session finished.");

    ws.exit().await;
    Ok(())
}
