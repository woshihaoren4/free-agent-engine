use std::path::PathBuf;

use fae_agent::*;

use crate::{SessionRuntime, UserMemoryRuntime};

/// Reads existing JSONL partitions and workspace files; creates no new storage format.
#[derive(Debug)]
pub struct SessionFileSource {
    home: PathBuf,
}

impl SessionFileSource {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }
}

#[async_trait::async_trait]
impl SessionDataSource for SessionFileSource {
    async fn read(&self, context: &ContextData) -> ControlResult<SessionStoredData> {
        let config = &context.session_config;
        let target = match &config.target {
            PlanTarget::Agent { agent_id } => agent_id,
            PlanTarget::Workflow { workflow_id } => workflow_id,
        };
        let error = |e: fae_agent::Error| SessionControlError::Invalid(e.to_string());
        let SessionResponse::History { messages, .. } = SessionRuntime::with_host_dir(&self.home)
            .query(
                target,
                context.user_info.user_id.as_str(),
                config.session_id.as_str(),
            )
            .await
            .map_err(error)?
        else {
            return Err(SessionControlError::Invalid(
                "expected history response".into(),
            ));
        };
        let UserMemoryResponse::Memories { memories, .. } =
            UserMemoryRuntime::with_host_dir(&self.home)
                .query(context.user_info.user_id.as_str())
                .await
                .map_err(error)?
        else {
            return Err(SessionControlError::Invalid(
                "expected memory response".into(),
            ));
        };
        let mut project_files = Vec::new();
        for name in ["workspace.md", "rules.md", "agents.md", "project.md"] {
            let path = PathBuf::from(&config.workspace.root)
                .join(".fae")
                .join(name);
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(content) => Some(content),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(SessionControlError::Invalid(error.to_string())),
            };
            project_files.push(WorkspaceFileView {
                path: path.to_string_lossy().into_owned(),
                content,
            });
        }
        Ok(SessionStoredData {
            history: messages,
            user_memories: memories,
            project_files,
        })
    }
}
