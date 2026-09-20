use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use fae_agent::{
    Event, EventType, RuntimeSelectExec, TaskError, TaskReq, TaskResp, TaskType, UserMemory,
    UserMemoryQuery, UserMemoryRequest, UserMemoryResponse,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::Mutex,
};
use wd_tools::channel::{Channel, Receiver, Sender};

#[derive(Debug)]
pub struct UserMemoryRuntime {
    host_dir: PathBuf,
    mutation_lock: Arc<Mutex<()>>,
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Default for UserMemoryRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl UserMemoryRuntime {
    pub const ID: &'static str = "user_memory_default";

    pub fn new() -> Self {
        Self::with_host_dir(super::default_fae_host())
    }

    pub fn with_host_dir(host_dir: impl Into<PathBuf>) -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            host_dir: host_dir.into(),
            mutation_lock: Arc::new(Mutex::new(())),
            event_sender,
            event_receiver,
        }
    }

    pub fn host_dir(&self) -> &Path {
        &self.host_dir
    }

    pub fn memory_path(&self, user_id: &str) -> fae_agent::Result<PathBuf> {
        validate_user_id(user_id)?;
        Ok(self
            .host_dir
            .join("memory")
            .join(format!("{user_id}.jsonl")))
    }

    pub async fn query(&self, user_id: &str) -> fae_agent::Result<UserMemoryResponse> {
        let path = self.memory_path(user_id)?;
        let memories = read_memories(&path).await?;
        Ok(UserMemoryResponse::Memories { path, memories })
    }

    pub async fn update(
        &self,
        user_id: &str,
        id: Option<u64>,
        category: fae_agent::UserMemoryCategory,
        content: String,
        confidence: fae_agent::UserMemoryConfidence,
    ) -> fae_agent::Result<UserMemoryResponse> {
        let _guard = self.mutation_lock.lock().await;
        let path = self.memory_path(user_id)?;
        if content.trim().is_empty() {
            return Err(anyhow::anyhow!("memory content cannot be empty").into());
        }

        let mut memories = read_memories(&path).await?;
        let updated_at = Utc::now().to_rfc3339();
        let (memory, created) = match id {
            Some(0) => return Err(anyhow::anyhow!("memory id must be positive").into()),
            Some(id) => {
                let memory = memories
                    .iter_mut()
                    .find(|memory| memory.id == id)
                    .ok_or_else(|| anyhow::anyhow!("memory id `{id}` does not exist"))?;
                memory.category = category;
                memory.content = content;
                memory.confidence = confidence;
                memory.updated_at = updated_at;
                (memory.clone(), false)
            }
            None => {
                let id = memories
                    .iter()
                    .map(|memory| memory.id)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("memory id overflow"))?;
                let memory = UserMemory {
                    id,
                    category,
                    content,
                    confidence,
                    updated_at,
                };
                memories.push(memory.clone());
                (memory, true)
            }
        };

        write_memories(&path, &memories).await?;
        Ok(UserMemoryResponse::Updated {
            path,
            memory,
            created,
        })
    }

    pub async fn delete(&self, user_id: &str, id: u64) -> fae_agent::Result<UserMemoryResponse> {
        let _guard = self.mutation_lock.lock().await;
        let path = self.memory_path(user_id)?;
        if id == 0 {
            return Err(anyhow::anyhow!("memory id must be positive").into());
        }

        let mut memories = read_memories(&path).await?;
        let index = memories
            .iter()
            .position(|memory| memory.id == id)
            .ok_or_else(|| anyhow::anyhow!("memory id `{id}` does not exist"))?;
        let memory = memories.remove(index);
        write_memories(&path, &memories).await?;
        Ok(UserMemoryResponse::Deleted { path, memory })
    }

    async fn execute(
        &self,
        task: TaskReq<UserMemoryRequest>,
    ) -> fae_agent::Result<TaskResp<UserMemoryResponse>> {
        let TaskReq { ctx, mut meta, req } = task;
        let resp = match req {
            UserMemoryRequest::Query { user_id } => self.query(&user_id).await?,
            UserMemoryRequest::Update {
                user_id,
                id,
                category,
                content,
                confidence,
            } => {
                self.update(&user_id, id, category, content, confidence)
                    .await?
            }
            UserMemoryRequest::Delete { user_id, id } => self.delete(&user_id, id).await?,
        };
        if meta.publisher.is_empty() {
            meta.publisher = Self::ID.to_string();
        }
        Ok(TaskResp { ctx, meta, resp })
    }
}

#[async_trait::async_trait]
impl RuntimeSelectExec<UserMemoryRequest, UserMemoryResponse, UserMemoryQuery, UserMemoryResponse>
    for UserMemoryRuntime
{
    fn id(&self) -> &str {
        Self::ID
    }

    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Memory]
    }

    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.event_receiver.clone())
    }

    async fn select(
        &self,
        ty: TaskType,
        query: UserMemoryQuery,
    ) -> fae_agent::Result<UserMemoryResponse> {
        if ty != TaskType::Memory {
            return Err(fae_agent::Error::RuntimeNoSupport);
        }
        self.query(&query.user_id).await
    }

    async fn spawn(&self, task: TaskReq<UserMemoryRequest>) -> fae_agent::Result<()> {
        let runtime = Self {
            host_dir: self.host_dir.clone(),
            mutation_lock: self.mutation_lock.clone(),
            event_sender: self.event_sender.clone(),
            event_receiver: self.event_receiver.clone(),
        };
        let event_sender = self.event_sender.clone();
        tokio::spawn(async move {
            let ctx = task.ctx.clone();
            let meta = task.meta.clone();
            let event_type = match runtime.execute(task).await {
                Ok(response) => EventType::TaskResult(response.into_response()),
                Err(error) => EventType::TaskError(TaskError {
                    ctx,
                    meta,
                    error: error.to_string(),
                }),
            };
            let _ = event_sender
                .send(Event {
                    from_rt_id: Self::ID.to_string(),
                    event_type,
                })
                .await;
        });
        Ok(())
    }

    async fn exec(
        &self,
        task: TaskReq<UserMemoryRequest>,
    ) -> fae_agent::Result<TaskResp<UserMemoryResponse>> {
        self.execute(task).await
    }
}

async fn read_memories(path: &Path) -> fae_agent::Result<Vec<UserMemory>> {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(anyhow::Error::from(error).into()),
    };

    let mut memories = Vec::new();
    let mut ids = HashSet::new();
    let mut lines = BufReader::new(file).lines();
    while let Some(line) = lines.next_line().await.map_err(anyhow::Error::from)? {
        if line.trim().is_empty() {
            continue;
        }
        let memory: UserMemory = serde_json::from_str(&line).map_err(anyhow::Error::from)?;
        if memory.id == 0 {
            return Err(anyhow::anyhow!("memory id must be positive").into());
        }
        if !ids.insert(memory.id) {
            return Err(anyhow::anyhow!("duplicate memory id `{}`", memory.id).into());
        }
        memories.push(memory);
    }
    Ok(memories)
}

async fn write_memories(path: &Path, memories: &[UserMemory]) -> fae_agent::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(anyhow::Error::from)?;
    }
    let mut output = Vec::new();
    for memory in memories {
        serde_json::to_writer(&mut output, memory).map_err(anyhow::Error::from)?;
        output.push(b'\n');
    }
    tokio::fs::write(path, output)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(())
}

fn validate_user_id(user_id: &str) -> fae_agent::Result<()> {
    let mut components = Path::new(user_id).components();
    if user_id.is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(anyhow::anyhow!("user_id must be a single non-empty path component").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::{
        ContextNull, Ctx, RuntimeSelectExec, TaskMeta, UserMemoryCategory, UserMemoryConfidence,
    };

    fn temp_host(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fae-memory-runtime-{name}-{}-{}",
            std::process::id(),
            wd_tools::uuid::v4()
        ))
    }

    #[tokio::test]
    async fn creates_queries_updates_and_deletes_user_memory() -> anyhow::Result<()> {
        let host = temp_host("crud");
        let runtime = UserMemoryRuntime::with_host_dir(&host);

        let first = runtime
            .update(
                "alice",
                None,
                UserMemoryCategory::Preference,
                "Prefers concise answers".to_string(),
                UserMemoryConfidence::UserStated,
            )
            .await?;
        let UserMemoryResponse::Updated {
            memory, created, ..
        } = first
        else {
            anyhow::bail!("expected updated response");
        };
        assert!(created);
        assert_eq!(memory.id, 1);

        let second = runtime
            .update(
                "alice",
                None,
                UserMemoryCategory::UserAttribute,
                "Name is Alice".to_string(),
                UserMemoryConfidence::UserStated,
            )
            .await?;
        let UserMemoryResponse::Updated { memory, .. } = second else {
            anyhow::bail!("expected updated response");
        };
        assert_eq!(memory.id, 2);

        runtime
            .update(
                "alice",
                Some(1),
                UserMemoryCategory::Preference,
                "Prefers concise technical answers".to_string(),
                UserMemoryConfidence::UserConfirmed,
            )
            .await?;

        let UserMemoryResponse::Memories { path, memories } = runtime.query("alice").await? else {
            anyhow::bail!("expected memories response");
        };
        assert_eq!(path, host.join("memory/alice.jsonl"));
        assert_eq!(memories.len(), 2);
        assert_eq!(memories[0].id, 1);
        assert_eq!(memories[0].confidence, UserMemoryConfidence::UserConfirmed);
        assert_eq!(memories[0].content, "Prefers concise technical answers");

        let UserMemoryResponse::Deleted { memory, .. } = runtime.delete("alice", 1).await? else {
            anyhow::bail!("expected deleted response");
        };
        assert_eq!(memory.id, 1);

        let UserMemoryResponse::Memories { memories, .. } = runtime.query("alice").await? else {
            anyhow::bail!("expected memories response");
        };
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].id, 2);

        let _ = tokio::fs::remove_dir_all(host).await;
        Ok(())
    }

    #[tokio::test]
    async fn missing_memory_file_returns_empty_and_rejects_bad_user_id() -> anyhow::Result<()> {
        let runtime = UserMemoryRuntime::with_host_dir(temp_host("missing"));
        let UserMemoryResponse::Memories { memories, .. } = runtime.query("alice").await? else {
            anyhow::bail!("expected memories response");
        };
        assert!(memories.is_empty());
        assert!(runtime.query("../alice").await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn delete_rejects_invalid_or_missing_memory_id() -> anyhow::Result<()> {
        let runtime = UserMemoryRuntime::with_host_dir(temp_host("delete-invalid"));
        assert!(runtime.delete("alice", 0).await.is_err());
        assert!(runtime.delete("alice", 1).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn spawned_task_preserves_plan_callback_publisher() -> anyhow::Result<()> {
        let runtime = UserMemoryRuntime::with_host_dir(temp_host("publisher"));
        let receiver = runtime.watch().await?;
        runtime
            .spawn(TaskReq {
                ctx: Ctx::new(Arc::new(ContextNull)),
                meta: TaskMeta {
                    id: "memory-task".to_string(),
                    plan_id: "single-agent-plan".to_string(),
                    ty: TaskType::Memory,
                    publisher: "plan_default".to_string(),
                    executor: UserMemoryRuntime::ID.to_string(),
                },
                req: UserMemoryRequest::Query {
                    user_id: "alice".to_string(),
                },
            })
            .await?;

        let event = receiver.recv().await?;
        let EventType::TaskResult(response) = event.event_type else {
            anyhow::bail!("expected task result");
        };
        assert_eq!(response.meta.publisher, "plan_default");
        Ok(())
    }
}
