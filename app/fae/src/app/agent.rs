use super::{
    args::{AgentArgs, ColorChoice},
    client::Client,
    paths,
};
use crate::{
    protocol::*,
    tui::{Mode, PromptAction, TerminalUi, UiAction},
};
use fae_agent::{SessionInput, SessionInputData, SingleAgentSource};
use std::path::{Path, PathBuf};

fn prompt(text: String, images: Vec<String>) -> anyhow::Result<(SessionInputData, Option<String>)> {
    let (text, mode) = if let Some(text) = text.strip_prefix("/todo ") {
        anyhow::ensure!(
            !text.trim().is_empty(),
            "Todo requirement cannot be empty. Use /todo <message>."
        );
        (text.trim().to_owned(), Some("todo".into()))
    } else {
        (text, None)
    };
    let images = images
        .iter()
        .map(|p| paths::attachment(p))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((SessionInputData::text(text).with_images(images), mode))
}

pub async fn run(
    args: AgentArgs,
    home: &Path,
    config: &ServerConfig,
    workspace: PathBuf,
    color: ColorChoice,
    no_alt_screen: bool,
) -> anyhow::Result<()> {
    let source = match (args.agent_config, args.agent_prompt) {
        (Some(config), Some(prompt)) => SingleAgentSource::Paths {
            config: paths::absolute(config)?.canonicalize()?,
            prompt: paths::absolute(prompt)?.canonicalize()?,
        },
        (None, None) => SingleAgentSource::AgentId(args.agent_id),
        _ => unreachable!("clap requires both paths"),
    };
    let mut open = OpenSession {
        binding: None,
        target: SessionTarget::Agent { source },
        session_id: args.session_id,
        workspace,
        resume: None,
    };
    let mut client = Client::open(home, config, open.clone()).await?;
    if !args.prompt.is_empty() {
        let (input, mode) = prompt(args.prompt.join(" "), vec![])?;
        let response = client
            .request(ClientMessage::Input {
                input: SessionInput::NewChat(input),
                mode,
            })
            .await?;
        if let ServerMessage::Ack { round_id } = response {
            client.info.status.round_id = round_id;
        }
        return super::output::stream(&mut client, false).await;
    }
    let mut ui = TerminalUi::new(
        Mode::Agent,
        &client.info.name,
        Some(client.info.user_id.clone()),
        &client.info.model,
        &client.info.session_id,
        color.enabled(),
        no_alt_screen,
    )?;
    loop {
        if client.info.status.round_id.is_some() {
            let result = run_round(&mut ui, &mut client, true).await;
            match result {
                Ok(false) => return Ok(()),
                Ok(true) => {}
                Err(error) => ui.push_error(format!("{error:#}")),
            }
        }
        let PromptAction::Submit { text, images } = ui.prompt().await? else {
            let _ = client
                .request(ClientMessage::Close {
                    cancel_active: false,
                })
                .await;
            return Ok(());
        };
        match text.as_str() {
            "/exit" | "/quit" => {
                let _ = client.request(ClientMessage::Close { cancel_active: false }).await;
                return Ok(());
            }
            "/clear" => ui.clear_transcript(),
            "/help" | "/session -h" | "/session --help" => ui.push_system(
                "/status  show session state\n/todo <message>  plan and execute with todos\n/session new  start a new session\n/session id=<id>  switch sessions\n/session clean, /clean  clear history and todos\n/clear  clear display\n/steer <message>  supplement a running agent\n/exit  leave the session"),
            "/status" => match client.request(ClientMessage::Status).await {
                Ok(status) => ui.push_system(format!("model: {}\nsession: {}\n{}", client.info.model, client.info.session_id, serde_json::to_string_pretty(&status)?)),
                Err(error) => ui.push_error(format!("{error:#}")),
            },
            "/session new" | "/clean" | "/session clean" => {
                let clean = text != "/session new";
                let result = client.request(if clean { ClientMessage::Clean } else { ClientMessage::Close { cancel_active: false } }).await;
                if let Err(error) = result { ui.push_error(format!("{error:#}")); continue; }
                open.session_id = Some(if clean { client.info.session_id.clone() } else { wd_tools::uuid::v4() });
                client = Client::open(home, config, open.clone()).await?;
                ui.clear_transcript();
                ui.set_subject(&client.info.session_id);
                ui.push_notice(format!("session: {}", client.info.session_id));
            }
            value if value.starts_with("/session id=") => {
                let id = value.trim_start_matches("/session id=");
                if id.is_empty() || id.contains(char::is_whitespace) {
                    ui.push_error("Invalid session ID");
                    continue;
                }
                let mut replacement = open.clone();
                replacement.session_id = Some(id.into());
                // Validate and open the new target before releasing the old lease.
                match Client::open(home, config, replacement.clone()).await {
                    Ok(new) => {
                        let _ = client.request(ClientMessage::Close { cancel_active: false }).await;
                        client = new;
                        open = replacement;
                        ui.clear_transcript();
                        ui.set_subject(&client.info.session_id);
                    }
                    Err(error) => ui.push_error(format!("{error:#}")),
                }
            }
            value if value.starts_with('/') && !value.starts_with("/todo ") => ui.push_error("Unknown command. Use /help."),
            _ => {
                let result = async {
                    let (input, mode) = prompt(text.clone(), images)?;
                    let response = client.request(ClientMessage::Input { input: SessionInput::NewChat(input), mode }).await?;
                    if let ServerMessage::Ack { round_id } = response { client.info.status.round_id = round_id; }
                    Ok::<_, anyhow::Error>(())
                }.await;
                match result {
                    Ok(()) => ui.push_user(text),
                    Err(error) => ui.push_error(format!("{error:#}")),
                }
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
    loop {
        tokio::select! {
            action = ui.running_action(can_steer), if !exiting => match action? {
                UiAction::Exit => {
                    let _ = client.request(ClientMessage::Close { cancel_active: false }).await;
                    return Ok(false);
                }
                UiAction::Cancel => {
                    exiting = true;
                    if let Some(round_id) = client.info.status.round_id.clone() {
                        if let Err(error) = client.request(ClientMessage::Cancel { round_id }).await { ui.push_error(format!("{error:#}")); }
                    }
                }
                UiAction::Steer(text) => {
                    match client.request(ClientMessage::Input { input: SessionInput::Supplement(SessionInputData::text(text.clone())), mode: None }).await {
                        Ok(_) => ui.push_steer(text),
                        Err(error) => ui.push_error(format!("{error:#}")),
                    }
                }
            },
            event = client.next() => match event? {
                ServerMessage::Event { output, .. } => { ui.apply_session_output(output)?; }
                ServerMessage::Finished { outcome, .. } => {
                    match outcome.result() {
                        Ok(result) if !can_steer => ui.workflow_result(&result),
                        Ok(_) => {},
                        Err(error) => ui.push_error(format!("{error:#}")),
                    }
                    return Ok(!exiting);
                }
                ServerMessage::Error(error) if error.code == "resume_gap" => {
                    ui.push_error(error.to_string());
                    if can_steer {
                        match client.request(ClientMessage::History { limit: 100, offset: 0 }).await {
                            Ok(ServerMessage::History { messages }) => ui.push_notice(format!(
                                "Persisted history (tool events may be missing):\n{}",
                                serde_json::to_string_pretty(&messages)?
                            )),
                            Err(error) => ui.push_error(format!("{error:#}")),
                            _ => {}
                        }
                    }
                    if client.info.status.round_id.is_none() {
                        return Err(error.into());
                    }
                }
                ServerMessage::Error(error) => return Err(error.into()),
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
        let (input, mode) = prompt("/todo   implement retries  ".into(), vec![]).unwrap();
        assert_eq!(input.text, "implement retries");
        assert_eq!(mode.as_deref(), Some("todo"));
        assert!(prompt("/todo  ".into(), vec![]).is_err());
        let (input, mode) = prompt("explain retries".into(), vec![]).unwrap();
        assert_eq!(input.text, "explain retries");
        assert!(mode.is_none());
    }
}
