use super::{
    args::{AgentArgs, ColorChoice},
    client::{Client, outcome_result, text_content},
    paths,
};
use crate::{
    protocol::ServerConfig,
    tui::{Mode, PromptAction, TerminalUi, UiAction},
};
use fae_agent::{AgentMode, ExecutionId, ExecutionOptions, PlanTarget, protocol::*};
use std::path::{Path, PathBuf};

fn prompt(text: String, images: Vec<String>) -> anyhow::Result<(Content, ExecutionOptions)> {
    let (text, mode) = if let Some(text) = text.strip_prefix("/todo ") {
        anyhow::ensure!(
            !text.trim().is_empty(),
            "Todo requirement cannot be empty. Use /todo <message>."
        );
        (text.trim().to_owned(), Some(AgentMode::Todo))
    } else {
        (text, None)
    };
    let mut content = text_content(text);
    for image in images {
        let uri = paths::attachment(&image)?;
        content.push(ContentBlock::Resource {
            resource: ResourceRef {
                uri,
                media_type: "image/*".into(),
                name: None,
            },
        });
    }
    Ok((content, ExecutionOptions { mode }))
}
async fn definition(home: &Path, args: &AgentArgs) -> anyhow::Result<String> {
    let (Some(config), Some(prompt)) = (&args.agent_config, &args.agent_prompt) else {
        return Ok(args.agent_id.clone());
    };
    let source = fae_agent::SingleAgentSource::Paths {
        config: paths::absolute(config.clone())?.canonicalize()?,
        prompt: paths::absolute(prompt.clone())?.canonicalize()?,
    };
    let builder = fae_agent::SingleAgentPlanBuilder::with_home_dir(home);
    let (config, prompt) = builder.load_config(&source).await?;
    let id = config.agent.name.clone();
    let dir = home.join("agents");
    tokio::fs::create_dir_all(&dir).await?;
    let config_path = dir.join(format!("{id}_config.json"));
    let prompt_path = dir.join(format!("{id}_prompt.txt"));
    if config_path.exists() || prompt_path.exists() {
        let (installed, installed_prompt) = builder
            .load_config(&fae_agent::SingleAgentSource::AgentId(id.clone()))
            .await?;
        anyhow::ensure!(
            serde_json::to_value(installed)? == serde_json::to_value(&config)?
                && installed_prompt == prompt,
            "Definition {id} already exists with different content; choose another Agent name"
        );
    } else {
        crate::server::lifecycle::atomic_write(
            &config_path,
            &serde_json::to_vec_pretty(&config)?,
            true,
        )?;
        crate::server::lifecycle::atomic_write(&prompt_path, prompt.as_bytes(), true)?;
    }
    Ok(id)
}
async fn start(
    client: &mut Client,
    input: Content,
    options: ExecutionOptions,
) -> anyhow::Result<()> {
    client
        .request(RequestPayload::SessionExecutionStart(
            ExecutionStartRequest {
                input,
                target: None,
                workspace: None,
                options: Some(options),
            },
        ))
        .await?;
    Ok(())
}
pub async fn run(
    args: AgentArgs,
    home: &Path,
    config: &ServerConfig,
    workspace: PathBuf,
    color: ColorChoice,
    no_alt_screen: bool,
) -> anyhow::Result<()> {
    let id = definition(home, &args).await?;
    let create = SessionCreateRequest {
        name: id.clone(),
        target: PlanTarget::Agent { agent_id: id },
        workspace: Some(workspace.to_string_lossy().into()),
        options: Default::default(),
        channels: vec![],
    };
    let mut client =
        Client::open(home, config, create.clone(), args.session_id, None, None).await?;
    if !args.prompt.is_empty() {
        let (input, options) = prompt(args.prompt.join(" "), vec![])?;
        start(&mut client, input, options).await?;
        return super::output::stream(&mut client, false).await;
    }
    let mut ui = TerminalUi::new(
        Mode::Agent,
        &client.info.session_config.name,
        Some(client.info.user_info.user_id.to_string()),
        "—",
        client.sid(),
        color.enabled(),
        no_alt_screen,
    )?;
    loop {
        if client.active().is_some() {
            match run_round(&mut ui, &mut client, true).await {
                Ok(false) => return Ok(()),
                Ok(true) => {}
                Err(error) => ui.push_error(format!("{error:#}")),
            }
        }
        let PromptAction::Submit { text, images } = ui.prompt().await? else {
            let _ = client.close().await;
            return Ok(());
        };
        match text.as_str() {
            "/exit" | "/quit" => { let _ = client.close().await; return Ok(()); }
            "/clear" => ui.clear_transcript(),
            "/stop" => { let active = client.active(); if let Err(error) = client.stop_round(active).await { ui.push_error(format!("{error:#}")); } }
            "/help" | "/session -h" | "/session --help" => ui.push_system(
                "/status  show session state\n/todo <message>  plan and execute\n/session new  create a session\n/session id=<id>  select an existing session\n/session clean, /clean  clear history and todos\n/clear  clear display\n/steer <message>  supplement a running agent\n/stop  stop the execution\n/exit  leave"),
            "/status" => match client.request(RequestPayload::SessionGet(Empty {})).await {
                Ok(status) => ui.push_system(serde_json::to_string_pretty(&status)?), Err(error) => ui.push_error(format!("{error:#}")),
            },
            "/clean" | "/session clean" => match client.request(RequestPayload::SessionConversationClear(Empty {})).await {
                Ok(_) => { ui.clear_transcript(); ui.push_notice(format!("session: {}", client.sid())); }
                Err(error) => ui.push_error(format!("{error:#}")),
            },
            "/session new" => {
                match Client::open(home, config, create.clone(), None, None, None).await {
                    Ok(new) => { let _ = client.close().await; client = new; ui.clear_transcript(); ui.set_subject(client.sid()); }
                    Err(error) => ui.push_error(format!("{error:#}")),
                }
            }
            value if value.starts_with("/session id=") => {
                let id = value.trim_start_matches("/session id=");
                match Client::open(home, config, create.clone(), Some(id.into()), None, None).await {
                    Ok(new) => { let _ = client.close().await; client = new; ui.clear_transcript(); ui.set_subject(client.sid()); }
                    Err(error) => ui.push_error(format!("{error:#}")),
                }
            }
            value if value.starts_with('/') && !value.starts_with("/todo ") => ui.push_error("Unknown command. Use /help."),
            _ => {
                let result = async { let (input, options) = prompt(text.clone(), images)?; start(&mut client, input, options).await }.await;
                match result { Ok(()) => ui.push_user(text), Err(error) => ui.push_error(format!("{error:#}")) }
            }
        }
    }
}
pub async fn run_round(
    ui: &mut TerminalUi,
    client: &mut Client,
    can_steer: bool,
) -> anyhow::Result<bool> {
    let mut exiting = false;
    let mut stopping = false;
    loop {
        tokio::select! {
            action = ui.running_action(can_steer), if !exiting && !stopping => match action? {
                UiAction::Exit => { let _ = client.close().await; return Ok(false); }
                UiAction::Cancel => { exiting = true; let active = client.active(); if let Err(error) = client.stop_round(active).await { ui.push_error(format!("{error:#}")); } }
                UiAction::Stop => {
                    let active = client.active();
                    match client.stop_round(active).await { Ok(_) => stopping = true, Err(error) => ui.push_error(format!("{error:#}")) }
                }
                UiAction::Steer(text) => {
                    if let Some(active) = client.active() {
                        match client.request(RequestPayload::SessionExecutionSteer(ExecutionSteerRequest { execution_id: ExecutionId::new(active)?, input: text_content(text.clone()) })).await {
                            Ok(_) => ui.push_steer(text), Err(error) => ui.push_error(format!("{error:#}")),
                        }
                    }
                }
            },
            event = client.next() => match event?.payload {
                MessagePayload::Event(EventPayload::OutputDelta(delta)) => {
                    ui.apply_output_delta(&delta)?;
                }
                MessagePayload::Event(EventPayload::InvocationChanged(changed)) => {
                    if let Some(model) = &changed.invocation.model {
                        ui.set_execution_model(Some(model));
                    }
                }
                MessagePayload::Event(EventPayload::ExecutionStateChanged(changed))
                    if changed.status == fae_agent::ExecutionStatus::Preparing => {
                    ui.set_execution_model(None);
                }
                MessagePayload::Event(EventPayload::ExecutionFinished(finished)) => {
                    ui.finish_execution();
                    if stopping && matches!(finished.outcome, fae_agent::ExecutionOutcome::Cancelled { .. }) { ui.push_notice("Stopped"); return Ok(true); }
                    match outcome_result(finished.outcome) {
                        Ok(result) if !can_steer => ui.workflow_result(&result), Ok(_) => {}, Err(error) => ui.push_error(format!("{error:#}")),
                    }
                    return Ok(!exiting);
                }
                _ => {}
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_todo_prompt_and_rejects_empty_requirement() {
        let (input, options) = prompt("/todo   implement retries  ".into(), vec![]).unwrap();
        assert_eq!(input, text_content("implement retries"));
        assert_eq!(options.mode, Some(AgentMode::Todo));
        assert!(prompt("/todo  ".into(), vec![]).is_err());
        assert!(
            prompt("explain retries".into(), vec![])
                .unwrap()
                .1
                .mode
                .is_none()
        );
    }
}
