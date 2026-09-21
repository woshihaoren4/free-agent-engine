use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use fae_agent::{SingleAgentHook, SingleAgentHookBuilder, SingleAgentHookContext};

const WORKSPACE_DESCRIPTION: &str = "Current project and workspace description";

#[derive(Clone, Debug)]
pub struct WorkspaceHookBuilder {
    workspace: PathBuf,
}

impl WorkspaceHookBuilder {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl SingleAgentHookBuilder for WorkspaceHookBuilder {
    async fn build(&self) -> Arc<dyn SingleAgentHook> {
        Arc::new(WorkspaceHook {
            workspace: self.workspace.clone(),
        })
    }
}

#[derive(Debug)]
struct WorkspaceHook {
    workspace: PathBuf,
}

#[async_trait::async_trait]
impl SingleAgentHook for WorkspaceHook {
    async fn on_prompt(
        &self,
        _ctx: &SingleAgentHookContext<'_>,
        prompt: String,
    ) -> anyhow::Result<String> {
        append_workspace_prompt(&self.workspace, prompt).await
    }
}

pub fn resolve_workspace(workspace: PathBuf) -> anyhow::Result<PathBuf> {
    let workspace = if workspace.is_absolute() {
        workspace
    } else {
        std::env::current_dir()
            .context("resolve current working directory")?
            .join(workspace)
    };
    let workspace = workspace.canonicalize().with_context(|| {
        format!(
            "resolve workspace directory `{}`",
            workspace.as_path().display()
        )
    })?;
    anyhow::ensure!(
        workspace.is_dir(),
        "workspace path `{}` is not a directory",
        workspace.display()
    );
    Ok(workspace)
}

async fn append_workspace_prompt(workspace: &Path, prompt: String) -> anyhow::Result<String> {
    let fae_dir = workspace.join(".fae");
    let workspace_path = fae_dir.join("workspace.md");
    let workspace_content = read_optional(&workspace_path).await?;
    let rules = read_optional(&fae_dir.join("rules.md")).await?;
    let agents = read_optional(&fae_dir.join("agents.md")).await?;
    let project = read_optional(&fae_dir.join("project.md")).await?;

    let is_empty =
        workspace_content.is_none() && rules.is_none() && agents.is_none() && project.is_none();

    let mut context = format!(
        "<Workspace desc=\"{WORKSPACE_DESCRIPTION}\",workspace_path=\"{}\">\n",
        escape_xml(&workspace.display().to_string())
    );
    push_indented(
        &mut context,
        &format!(
            "Workspace description file path: {}",
            workspace_path.as_path().display()
        ),
    );
    if is_empty {
        push_indented(
            &mut context,
            "No workspace context files were found. Add project-specific context to workspace.md.",
        );
    } else {
        push_indented(&mut context, "Workspace context:");
        push_indented(
            &mut context,
            workspace_content.as_deref().unwrap_or_default(),
        );
        push_section(&mut context, "rules", rules.as_deref().unwrap_or_default());
        push_section(
            &mut context,
            "agents",
            agents.as_deref().unwrap_or_default(),
        );
        push_section(
            &mut context,
            "project",
            project.as_deref().unwrap_or_default(),
        );
    }
    context.push_str("</Workspace>");

    if prompt.is_empty() {
        Ok(context)
    } else {
        Ok(format!("{prompt}\n\n{context}"))
    }
}

async fn read_optional(path: &Path) -> anyhow::Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read `{}`", path.display())),
    }
}

fn push_section(output: &mut String, name: &str, content: &str) {
    push_indented(output, &format!("<{name}>"));
    push_indented(output, content);
    push_indented(output, &format!("</{name}>"));
}

fn push_indented(output: &mut String, content: &str) {
    if content.is_empty() {
        output.push_str("    \n");
        return;
    }
    for line in content.lines() {
        output.push_str("    ");
        output.push_str(line);
        output.push('\n');
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> PathBuf {
        std::env::temp_dir().join(format!(
            "fae-workspace-{}-{}",
            std::process::id(),
            wd_tools::uuid::v4()
        ))
    }

    #[tokio::test]
    async fn appends_empty_workspace_without_workspace_files() {
        let workspace = temp_workspace();
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let prompt = append_workspace_prompt(&workspace, "base prompt".to_string())
            .await
            .unwrap();

        assert!(prompt.starts_with(
            "base prompt\n\n<Workspace desc=\"Current project and workspace description\""
        ));
        assert!(prompt.contains(&format!(
            "Workspace description file path: {}",
            workspace.join(".fae/workspace.md").display()
        )));
        assert!(prompt.contains(
            "No workspace context files were found. Add project-specific context to workspace.md."
        ));
        assert!(!prompt.contains("<rules>"));
        assert!(prompt.ends_with("</Workspace>"));
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn appends_existing_workspace_files() {
        let workspace = temp_workspace();
        let fae_dir = workspace.join(".fae");
        tokio::fs::create_dir_all(&fae_dir).await.unwrap();
        tokio::fs::write(fae_dir.join("workspace.md"), "workspace line")
            .await
            .unwrap();
        tokio::fs::write(fae_dir.join("rules.md"), "rule one\nrule two")
            .await
            .unwrap();
        tokio::fs::write(fae_dir.join("agents.md"), "agent content")
            .await
            .unwrap();
        tokio::fs::write(fae_dir.join("project.md"), "project content")
            .await
            .unwrap();

        let prompt = append_workspace_prompt(&workspace, "base prompt".to_string())
            .await
            .unwrap();

        assert!(prompt.starts_with(
            "base prompt\n\n<Workspace desc=\"Current project and workspace description\""
        ));
        assert!(prompt.contains(&format!("workspace_path=\"{}\"", workspace.display())));
        assert!(prompt.contains(&format!(
            "Workspace description file path: {}",
            fae_dir.join("workspace.md").display()
        )));
        assert!(prompt.contains("    workspace line\n"));
        assert!(prompt.contains("    <rules>\n    rule one\n    rule two\n    </rules>\n"));
        assert!(prompt.contains("    <agents>\n    agent content\n    </agents>\n"));
        assert!(prompt.contains("    <project>\n    project content\n    </project>\n"));
        assert!(prompt.ends_with("</Workspace>"));
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn includes_empty_sections_for_missing_files() {
        let workspace = temp_workspace();
        let fae_dir = workspace.join(".fae");
        tokio::fs::create_dir_all(&fae_dir).await.unwrap();
        tokio::fs::write(fae_dir.join("rules.md"), "only rules")
            .await
            .unwrap();

        let prompt = append_workspace_prompt(&workspace, String::new())
            .await
            .unwrap();

        assert!(prompt.contains("    <rules>\n    only rules\n    </rules>\n"));
        assert!(prompt.contains("    <agents>\n    \n    </agents>\n"));
        assert!(prompt.contains("    <project>\n    \n    </project>\n"));
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }
}
