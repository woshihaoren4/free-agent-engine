use std::io::{self, Write};

use fae_agent::{protocol::ContentBlock, *};
use fae_engine::{Engine, SessionCoordinator};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = Engine::default().await;
    let target = PlanTarget::Agent {
        agent_id: "workspace-assistant".into(),
    };
    let workspace = std::env::current_dir()?.to_string_lossy().into_owned();
    let session = SessionCoordinator::new(
        engine.clone(),
        ContextData {
            user_info: UserInfo {
                user_id: UserId::new(DEFAULT_USER_ID)?,
                name: None,
            },
            session_config: SessionConfig {
                session_id: SessionId::new(wd_tools::uuid::v4())?,
                name: "Interactive example".into(),
                lifecycle: SessionLifecycle::Open,
                created_at: Timestamp::now(),
                updated_at: Timestamp::now(),
                target: target.clone(),
                workspace: WorkspaceBinding {
                    root: workspace.clone(),
                    project_files: vec![],
                },
                options: ExecutionOptions::default(),
                channels: vec![],
            },
            memory_view: MemoryView::default(),
            latest_execution_view: None,
        },
    )?;
    println!("Enter a message. Use /exit or /quit to stop.");
    while let Some(input) = read_user_input()? {
        let execution = session.prepare(ExecutionSettings {
            target: target.clone(),
            workspace: workspace.clone(),
            options: ExecutionOptions::default(),
            definitions: vec![],
        })?;
        let execution_id = execution.execution_id.clone();
        let mut events = session.subscribe();
        let (env, _) = SingleAgentEnv::from_agent_id("workspace-assistant", input);
        let env = env.with_session_id(session.key().session_id.to_string());
        let runner = engine.clone();
        let job = tokio::spawn(async move { runner.execute_session(execution, env).await });
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => { session.stop(&execution_id, "Interrupted".into())?; }
                event = events.recv() => match event {
                    Some(SessionControlEvent::Output { content, .. }) => {
                        for block in content {
                            if let ContentBlock::Text { text } = block { print!("{text}"); }
                        }
                        io::stdout().flush()?;
                    }
                    Some(SessionControlEvent::Finished { outcome, .. }) => {
                        println!();
                        match outcome {
                            ExecutionOutcome::Failed { error } => eprintln!("{}", error.message),
                            ExecutionOutcome::Cancelled { reason } => eprintln!("{reason}"),
                            ExecutionOutcome::Succeeded { .. } => {}
                        }
                        break;
                    }
                    None => anyhow::bail!("Session event channel closed"),
                    _ => {}
                }
            }
        }
        let _ = job.await?;
        // The public result remains available after the runner and its resources exit.
        assert!(session.execution(Some(&execution_id))?.outcome.is_some());
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
            return Ok(None);
        }
        let input = input.trim();
        if matches!(input, "/exit" | "/quit") {
            return Ok(None);
        }
        if !input.is_empty() {
            return Ok(Some(input.to_owned()));
        }
    }
}
