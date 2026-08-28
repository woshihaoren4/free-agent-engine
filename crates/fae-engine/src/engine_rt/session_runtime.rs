use std::path::{Component, Path, PathBuf};

use fae_agent::{Event, EventType, RuntimeSelectExec, TaskReq, TaskResp, TaskType};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use wd_tools::channel::{Channel, Receiver, Sender};

pub const FAE_HOST_ENV: &str = "FAE_HOST";
pub const DEFAULT_FAE_HOST_DIR: &str = ".fae";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: SessionMessageRole,
    pub content: String,
}

impl SessionMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: SessionMessageRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: SessionMessageRole::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionQuery {
    pub user: String,
    pub session_id: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

impl SessionQuery {
    pub fn new(user: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            session_id: session_id.into(),
            limit: None,
            offset: None,
        }
    }

    pub fn with_page(
        user: impl Into<String>,
        session_id: impl Into<String>,
        limit: impl Into<Option<usize>>,
        offset: impl Into<Option<usize>>,
    ) -> Self {
        Self {
            user: user.into(),
            session_id: session_id.into(),
            limit: limit.into(),
            offset: offset.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionRequest {
    Add {
        user: String,
        session_id: String,
        messages: Vec<SessionMessage>,
    },
    Delete {
        user: String,
        session_id: String,
    },
    Query {
        user: String,
        session_id: String,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionResponse {
    Added {
        path: PathBuf,
        added: usize,
    },
    Deleted {
        path: PathBuf,
        existed: bool,
    },
    History {
        path: PathBuf,
        messages: Vec<SessionMessage>,
    },
}

#[derive(Debug)]
pub struct SessionRuntime {
    host_dir: PathBuf,
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Default for SessionRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRuntime {
    pub const ID: &'static str = "session_default";

    pub fn new() -> Self {
        Self::with_host_dir(default_fae_host())
    }

    pub fn with_host_dir(host_dir: impl Into<PathBuf>) -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            host_dir: host_dir.into(),
            event_sender,
            event_receiver,
        }
    }

    pub fn host_dir(&self) -> &Path {
        &self.host_dir
    }

    pub fn session_path(&self, user: &str, session_id: &str) -> fae_agent::Result<PathBuf> {
        validate_path_segment("user", user)?;
        validate_path_segment("session_id", session_id)?;

        Ok(self
            .host_dir
            .join("memory")
            .join(user)
            .join("session")
            .join(format!("{session_id}.jsonl")))
    }

    pub async fn add(
        &self,
        user: &str,
        session_id: &str,
        messages: &[SessionMessage],
    ) -> fae_agent::Result<SessionResponse> {
        let path = self.session_path(user, session_id)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(anyhow::Error::from)?;
        }

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(anyhow::Error::from)?;

        for message in messages {
            let mut line = serde_json::to_vec(message).map_err(anyhow::Error::from)?;
            line.push(b'\n');
            file.write_all(&line).await.map_err(anyhow::Error::from)?;
        }

        file.flush().await.map_err(anyhow::Error::from)?;

        Ok(SessionResponse::Added {
            path,
            added: messages.len(),
        })
    }

    pub async fn delete(&self, user: &str, session_id: &str) -> fae_agent::Result<SessionResponse> {
        let path = self.session_path(user, session_id)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(SessionResponse::Deleted {
                path,
                existed: true,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(SessionResponse::Deleted {
                    path,
                    existed: false,
                })
            }
            Err(err) => Err(anyhow::Error::from(err).into()),
        }
    }

    pub async fn query(&self, user: &str, session_id: &str) -> fae_agent::Result<SessionResponse> {
        self.query_page(user, session_id, None, None).await
    }

    pub async fn query_page(
        &self,
        user: &str,
        session_id: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> fae_agent::Result<SessionResponse> {
        let path = self.session_path(user, session_id)?;
        let file = match tokio::fs::File::open(&path).await {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SessionResponse::History {
                    path,
                    messages: Vec::new(),
                });
            }
            Err(err) => return Err(anyhow::Error::from(err).into()),
        };

        let mut messages = Vec::new();
        let mut lines = BufReader::new(file).lines();
        let offset = offset.unwrap_or(0);
        let mut seen = 0usize;
        while let Some(line) = lines.next_line().await.map_err(anyhow::Error::from)? {
            if line.trim().is_empty() {
                continue;
            }

            if seen < offset {
                seen += 1;
                continue;
            }

            if limit.is_some_and(|limit| messages.len() >= limit) {
                break;
            }

            messages
                .push(serde_json::from_str::<SessionMessage>(&line).map_err(anyhow::Error::from)?);
            seen += 1;
        }

        Ok(SessionResponse::History { path, messages })
    }

    async fn exec_session(
        &self,
        task: TaskReq<SessionRequest>,
    ) -> fae_agent::Result<TaskResp<SessionResponse>> {
        let TaskReq { ctx, mut meta, req } = task;
        let resp = match req {
            SessionRequest::Add {
                user,
                session_id,
                messages,
            } => self.add(&user, &session_id, &messages).await?,
            SessionRequest::Delete { user, session_id } => self.delete(&user, &session_id).await?,
            SessionRequest::Query {
                user,
                session_id,
                limit,
                offset,
            } => self.query_page(&user, &session_id, limit, offset).await?,
        };

        meta.publisher = Self::ID.to_string();
        Ok(TaskResp { ctx, meta, resp })
    }
}

#[async_trait::async_trait]
impl RuntimeSelectExec<SessionRequest, SessionResponse, SessionQuery, SessionResponse>
    for SessionRuntime
{
    fn id(&self) -> &str {
        Self::ID
    }

    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Session]
    }

    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.event_receiver.clone())
    }

    async fn select(&self, ty: TaskType, cond: SessionQuery) -> fae_agent::Result<SessionResponse> {
        if ty != TaskType::Session {
            return Err(fae_agent::Error::RuntimeNoSupport);
        }

        self.query_page(&cond.user, &cond.session_id, cond.limit, cond.offset)
            .await
    }

    async fn spawn(&self, task: TaskReq<SessionRequest>) -> fae_agent::Result<()> {
        let event_sender = self.event_sender.clone();
        let host_dir = self.host_dir.clone();

        tokio::spawn(async move {
            let TaskReq { ctx, mut meta, req } = task;
            let response_ctx = ctx.clone();
            let runtime = SessionRuntime::with_host_dir(host_dir);
            let result = match req {
                SessionRequest::Add {
                    user,
                    session_id,
                    messages,
                } => runtime.add(&user, &session_id, &messages).await,
                SessionRequest::Delete { user, session_id } => {
                    runtime.delete(&user, &session_id).await
                }
                SessionRequest::Query {
                    user,
                    session_id,
                    limit,
                    offset,
                } => runtime.query_page(&user, &session_id, limit, offset).await,
            }
            .map(|resp| {
                meta.publisher = Self::ID.to_string();
                Event {
                    from_rt_id: Self::ID.to_string(),
                    event_type: EventType::TaskResult(
                        TaskResp {
                            ctx: response_ctx,
                            meta,
                            resp,
                        }
                        .into_response(),
                    ),
                }
            });

            match result {
                Ok(event) => {
                    if let Err(err) = event_sender.send(event).await {
                        wd_log::log_error_ln!("send session task result failed: {:?}", err);
                    }
                }
                Err(err) => ctx.error(err.to_string()),
            }
        });

        Ok(())
    }

    async fn exec(
        &self,
        task: TaskReq<SessionRequest>,
    ) -> fae_agent::Result<TaskResp<SessionResponse>> {
        self.exec_session(task).await
    }
}

pub fn default_fae_host() -> PathBuf {
    match std::env::var_os(FAE_HOST_ENV) {
        Some(host) if !host.is_empty() => expand_home(PathBuf::from(host)),
        _ => home_dir()
            .map(|home| home.join(DEFAULT_FAE_HOST_DIR))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_FAE_HOST_DIR)),
    }
}

fn expand_home(path: PathBuf) -> PathBuf {
    let Some(path_str) = path.to_str() else {
        return path;
    };

    if path_str == "~" {
        return home_dir().unwrap_or(path);
    }

    if let Some(rest) = path_str.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }

    path
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
        })
}

fn validate_path_segment(name: &str, segment: &str) -> fae_agent::Result<()> {
    if segment.is_empty() {
        return Err(anyhow::anyhow!("{name} must not be empty").into());
    }

    if segment.contains('/') || segment.contains('\\') {
        return Err(anyhow::anyhow!("{name} must be a single path segment").into());
    }

    let path = Path::new(segment);
    if path.components().count() != 1 {
        return Err(anyhow::anyhow!("{name} must be a single path segment").into());
    }

    match path.components().next() {
        Some(Component::Normal(_)) => Ok(()),
        _ => Err(anyhow::anyhow!("{name} must be a normal path segment").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::{ContextNull, Ctx, TaskMeta};
    use std::sync::Arc;

    fn task(req: SessionRequest) -> TaskReq<SessionRequest> {
        TaskReq {
            ctx: Ctx::new(Arc::new(ContextNull)),
            meta: TaskMeta {
                ty: TaskType::Session,
                ..Default::default()
            },
            req,
        }
    }

    #[tokio::test]
    async fn test_session_runtime_add_query_delete_bits_ut() -> anyhow::Result<()> {
        let host = std::env::temp_dir().join(format!("fae-session-runtime-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&host).await;
        let runtime = SessionRuntime::with_host_dir(&host);

        let response = runtime
            .exec(task(SessionRequest::Add {
                user: "user-1".to_string(),
                session_id: "session-1".to_string(),
                messages: vec![
                    SessionMessage::user("hello"),
                    SessionMessage::assistant("hi"),
                ],
            }))
            .await?;
        assert_eq!(response.meta.publisher, SessionRuntime::ID);
        assert_eq!(
            response.resp,
            SessionResponse::Added {
                path: host
                    .join("memory")
                    .join("user-1")
                    .join("session")
                    .join("session-1.jsonl"),
                added: 2,
            }
        );

        let response = runtime
            .exec(task(SessionRequest::Query {
                user: "user-1".to_string(),
                session_id: "session-1".to_string(),
                limit: None,
                offset: None,
            }))
            .await?;
        let SessionResponse::History { messages, .. } = response.resp else {
            anyhow::bail!("expected history response");
        };
        assert_eq!(
            messages,
            vec![
                SessionMessage::user("hello"),
                SessionMessage::assistant("hi"),
            ]
        );

        let content = tokio::fs::read_to_string(
            host.join("memory")
                .join("user-1")
                .join("session")
                .join("session-1.jsonl"),
        )
        .await?;
        assert_eq!(
            content,
            "{\"role\":\"user\",\"content\":\"hello\"}\n{\"role\":\"assistant\",\"content\":\"hi\"}\n"
        );

        let response = runtime
            .exec(task(SessionRequest::Delete {
                user: "user-1".to_string(),
                session_id: "session-1".to_string(),
            }))
            .await?;
        assert!(matches!(
            response.resp,
            SessionResponse::Deleted { existed: true, .. }
        ));

        let response = runtime
            .exec(task(SessionRequest::Query {
                user: "user-1".to_string(),
                session_id: "session-1".to_string(),
                limit: None,
                offset: None,
            }))
            .await?;
        assert!(matches!(
            response.resp,
            SessionResponse::History { messages, .. } if messages.is_empty()
        ));

        let _ = tokio::fs::remove_dir_all(&host).await;
        Ok(())
    }

    #[tokio::test]
    async fn test_session_runtime_select_queries_history_bits_ut() -> anyhow::Result<()> {
        let host =
            std::env::temp_dir().join(format!("fae-session-runtime-select-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&host).await;
        let runtime = SessionRuntime::with_host_dir(&host);

        runtime
            .add(
                "user-1",
                "session-1",
                &[
                    SessionMessage::user("question"),
                    SessionMessage::assistant("answer"),
                ],
            )
            .await?;

        let response = runtime
            .select(TaskType::Session, SessionQuery::new("user-1", "session-1"))
            .await?;

        let SessionResponse::History { messages, .. } = response else {
            anyhow::bail!("expected history response");
        };
        assert_eq!(
            messages,
            vec![
                SessionMessage::user("question"),
                SessionMessage::assistant("answer"),
            ]
        );

        let _ = tokio::fs::remove_dir_all(&host).await;
        Ok(())
    }

    #[tokio::test]
    async fn test_session_runtime_query_supports_limit_offset_bits_ut() -> anyhow::Result<()> {
        let host =
            std::env::temp_dir().join(format!("fae-session-runtime-page-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&host).await;
        let runtime = SessionRuntime::with_host_dir(&host);

        runtime
            .add(
                "user-1",
                "session-1",
                &[
                    SessionMessage::user("m0"),
                    SessionMessage::assistant("m1"),
                    SessionMessage::user("m2"),
                    SessionMessage::assistant("m3"),
                ],
            )
            .await?;

        let response = runtime
            .select(
                TaskType::Session,
                SessionQuery::with_page("user-1", "session-1", Some(2), Some(1)),
            )
            .await?;

        let SessionResponse::History { messages, .. } = response else {
            anyhow::bail!("expected history response");
        };
        assert_eq!(
            messages,
            vec![SessionMessage::assistant("m1"), SessionMessage::user("m2"),]
        );

        let response = runtime
            .exec(task(SessionRequest::Query {
                user: "user-1".to_string(),
                session_id: "session-1".to_string(),
                limit: Some(1),
                offset: Some(3),
            }))
            .await?;
        assert!(matches!(
            response.resp,
            SessionResponse::History { messages, .. } if messages == vec![SessionMessage::assistant("m3")]
        ));

        let _ = tokio::fs::remove_dir_all(&host).await;
        Ok(())
    }

    #[test]
    fn test_session_path_rejects_nested_segments_bits_ut() {
        let runtime = SessionRuntime::with_host_dir("/tmp/fae-session-runtime");

        assert!(runtime.session_path("../user", "session-1").is_err());
        assert!(runtime.session_path("user-1", "nested/session").is_err());
        assert!(runtime.session_path("user-1", "").is_err());
    }
}
