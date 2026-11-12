use super::{
    args::{ColorChoice, WorkflowArgs},
    client::Client,
    paths,
};
use crate::{
    protocol::*,
    tui::{Mode, TerminalUi},
};
use std::path::{Path, PathBuf};

async fn input(value: &str) -> anyhow::Result<serde_json::Value> {
    let text = if let Some(path) = value.strip_prefix('@') {
        anyhow::ensure!(!path.is_empty(), "workflow input path cannot be empty");
        tokio::fs::read_to_string(paths::absolute(path.into())?).await?
    } else {
        value.into()
    };
    Ok(serde_json::from_str(&text)?)
}
pub async fn run(
    args: WorkflowArgs,
    home: &Path,
    config: &ServerConfig,
    workspace: PathBuf,
    color: ColorChoice,
    no_alt_screen: bool,
) -> anyhow::Result<()> {
    let open = OpenSession {
        binding: None,
        target: SessionTarget::Workflow {
            workflow_id: args.id.clone(),
            input: input(&args.input).await?,
        },
        session_id: None,
        workspace: Some(workspace),
        resume: None,
    };
    let mut client = Client::open(home, config, open).await?;
    if !args.interactive {
        return super::output::stream(&mut client, true).await;
    }
    let mut ui = TerminalUi::new(
        Mode::Workflow,
        &client.info.name,
        None,
        &client.info.model,
        &client.info.session_id,
        color.enabled(),
        no_alt_screen,
    )?;
    if super::agent::run_round(&mut ui, &mut client, false).await? {
        ui.wait_for_close().await?;
    }
    let _ = client
        .request(ClientMessage::Close {
            cancel_active: false,
        })
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn parses_inline_and_file_workflow_input() {
        assert_eq!(
            input(r#"{"enabled":true}"#).await.unwrap(),
            serde_json::json!({"enabled":true})
        );
        let path = std::env::temp_dir().join(format!("fae-input-{}.json", wd_tools::uuid::v4()));
        tokio::fs::write(&path, r#"{"count":3}"#).await.unwrap();
        assert_eq!(
            input(&format!("@{}", path.display())).await.unwrap(),
            serde_json::json!({"count":3})
        );
        tokio::fs::remove_file(path).await.unwrap();
    }
}
