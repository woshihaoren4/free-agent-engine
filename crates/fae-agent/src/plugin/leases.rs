//! Typed boundary resources. References grant access only within one Context tree
//! and immutable engine generation; ordinary JSON is never scanned for handles.
use super::bridge;
use crate::*;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};
use tokio_stream::StreamExt;

enum Resource {
    Model(ModelResponse),
    Tool(ToolResponse),
    Invocation(ToolInvocation),
    Remote(Remote),
}
#[async_trait::async_trait]
pub trait BoundaryStream: Send + Sync {
    async fn next(&self) -> anyhow::Result<Value>;
    async fn close(&self) -> anyhow::Result<()>;
}
struct Remote {
    source: Arc<dyn BoundaryStream>,
    ctx: Ctx,
    meta: TaskMeta,
    finished: bool,
}
impl Remote {
    fn finish(&mut self, state: ContextState, phase: ContextStackPhase) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.ctx.set_state(state).is_ok() {
            let _ = self
                .ctx
                .append_stack(ContextStackDraft::new(self.meta.clone(), phase));
        }
        let _ = self.ctx.invocation_finished();
    }
}
impl Drop for Remote {
    fn drop(&mut self) {
        let needs_close = !self.finished;
        self.finish(
            ContextState::cancelled("stream closed"),
            ContextStackPhase::Cancelled,
        );
        if needs_close {
            let source = self.source.clone();
            tokio::spawn(async move {
                let _ = source.close().await;
            });
        }
    }
}
pub fn register_stream(
    ctx: &Ctx,
    meta: TaskMeta,
    source: Arc<dyn BoundaryStream>,
) -> anyhow::Result<String> {
    insert(
        ctx,
        Resource::Remote(Remote {
            source,
            ctx: ctx.clone(),
            meta,
            finished: false,
        }),
    )
}
pub fn has_resources(ctx: &Ctx) -> bool {
    let Ok(scope) = ctx.scope() else {
        return false;
    };
    leases().lock().unwrap().values().any(|lease| {
        lease.scope == scope.scope_id && lease.generation == ctx.get_engine().generation()
    })
}
pub fn has_owned_resources(ctx: &Ctx) -> bool {
    leases().lock().unwrap().values().any(|lease| {
        lease.generation == ctx.get_engine().generation() && ctx.owns_scope(&lease.scope)
    })
}
pub fn has_remote(ctx: &Ctx) -> bool {
    let Ok(scope) = ctx.scope() else {
        return false;
    };
    leases().lock().unwrap().values().any(|lease| {
        lease.scope == scope.scope_id
            && lease.generation == ctx.get_engine().generation()
            && lease
                .resource
                .try_lock()
                .is_ok_and(|r| matches!(r.as_ref(), Some(Resource::Remote(_))))
    })
}
struct Reader {
    ctx: Ctx,
    id: String,
}
impl Drop for Reader {
    fn drop(&mut self) {
        let ctx = self.ctx.clone();
        let id = self.id.clone();
        tokio::spawn(async move {
            let _ = close(&ctx, &id).await;
        });
    }
}
struct Lease {
    generation: String,
    scope: String,
    resource: tokio::sync::Mutex<Option<Resource>>,
    closed: tokio::sync::watch::Sender<bool>,
    transferred: std::sync::atomic::AtomicBool,
}
static LEASES: OnceLock<Mutex<HashMap<String, Arc<Lease>>>> = OnceLock::new();
fn leases() -> &'static Mutex<HashMap<String, Arc<Lease>>> {
    LEASES.get_or_init(Mutex::default)
}
fn insert(ctx: &Ctx, resource: Resource) -> anyhow::Result<String> {
    let scope = ctx.scope()?.scope_id.clone();
    let id = wd_tools::uuid::v4();
    let lease = Arc::new(Lease {
        generation: ctx.get_engine().generation().into(),
        scope,
        resource: tokio::sync::Mutex::new(Some(resource)),
        closed: tokio::sync::watch::channel(false).0,
        transferred: std::sync::atomic::AtomicBool::new(false),
    });
    let mut released = lease.closed.subscribe();
    {
        let mut entries = leases().lock().unwrap();
        anyhow::ensure!(entries.len() < 1024, "boundary resource capacity exceeded");
        entries.insert(id.clone(), lease);
    }
    // Bounded retention also breaks the resource -> Context -> Engine cycle.
    let cleanup_id = id.clone();
    let weak = ctx.downgrade();
    tokio::spawn(async move {
        tokio::select! {
            _ = released.changed() => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(120)) => {}
            _ = async {
                if let Some(ctx) = weak.upgrade() { ctx.cancellation().await; }
                else { std::future::pending::<()>().await; }
            } => {}
        }
        let removed = leases().lock().unwrap().remove(&cleanup_id);
        if let Some(lease) = removed {
            lease.closed.send_replace(true);
            lease.resource.lock().await.take();
        }
    });
    Ok(id)
}
fn authorized(ctx: &Ctx, id: &str) -> anyhow::Result<Arc<Lease>> {
    let lease = leases()
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
    anyhow::ensure!(
        lease.generation == ctx.get_engine().generation() && ctx.owns_scope(&lease.scope),
        "HandleExpired"
    );
    Ok(lease)
}
fn take(ctx: &Ctx, id: &str, model: bool) -> anyhow::Result<Resource> {
    let lease = authorized(ctx, id)?;
    let mut resource = lease
        .resource
        .try_lock()
        .map_err(|_| anyhow::anyhow!("resource busy"))?;
    anyhow::ensure!(
        matches!(resource.as_ref(), Some(Resource::Model(_))) == model
            && matches!(
                resource.as_ref(),
                Some(Resource::Model(_) | Resource::Tool(_))
            ),
        "resource type mismatch"
    );
    let result = resource
        .take()
        .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
    leases().lock().unwrap().remove(id);
    lease.closed.send_replace(true);
    Ok(result)
}
pub async fn next(ctx: &Ctx, id: &str) -> anyhow::Result<Value> {
    let lease = authorized(ctx, id)?;
    let mut resource = lease
        .resource
        .try_lock()
        .map_err(|_| anyhow::anyhow!("stream busy"))?;
    let mut closed = lease.closed.subscribe();
    if *closed.borrow() {
        anyhow::bail!("HandleExpired");
    }
    let outcome = tokio::select! {
        biased;
        _ = closed.changed() => Err(anyhow::anyhow!("stream closed")),
        _ = ctx.cancellation() => Err(anyhow::anyhow!("stream cancelled")),
        result = next_item(&mut resource) => result,
    };
    match outcome {
        Ok((value, done)) => {
            if done {
                resource.take();
                leases().lock().unwrap().remove(id);
                lease.closed.send_replace(true);
            }
            Ok(value)
        }
        Err(error) => {
            if !*closed.borrow() {
                resource.take();
                leases().lock().unwrap().remove(id);
                lease.closed.send_replace(true);
            }
            Err(error)
        }
    }
}
async fn next_item(resource: &mut Option<Resource>) -> anyhow::Result<(Value, bool)> {
    Ok(match resource.as_mut() {
        Some(Resource::Model(ModelResponse::Streaming(stream))) => match stream.next().await {
            Some(Ok(chunk)) => (json!({"done":false,"value":chunk}), false),
            Some(Err(error)) => return Err(error.into()),
            None => (json!({"done":true}), true),
        },
        Some(Resource::Tool(tool)) => match tool.next().await {
            Ok(ToolRespItem::Streaming(value)) => {
                (json!({"done":false,"value":{"Streaming":value}}), false)
            }
            Ok(ToolRespItem::Completed(value)) => {
                (json!({"done":true,"value":{"Completed":value}}), true)
            }
            Err(error) => return Err(error),
        },
        Some(Resource::Remote(remote)) => match remote.source.next().await {
            Ok(mut value) => {
                let done = value
                    .get("done")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| anyhow::anyhow!("invalid stream item"))?
                    || (remote.meta.ty == TaskType::Tool
                        && value
                            .get("value")
                            .is_some_and(|v| v.get("Completed").is_some()));
                if done {
                    remote.finish(ContextState::succeeded(()), ContextStackPhase::Succeeded);
                }
                value["done"] = Value::Bool(done);
                (value, done)
            }
            Err(error) => {
                remote.finish(
                    ContextState::failed(error.to_string()),
                    ContextStackPhase::Failed,
                );
                return Err(error);
            }
        },
        _ => anyhow::bail!("resource is not a stream"),
    })
}
pub async fn close(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    let lease = authorized(ctx, id)?;
    leases().lock().unwrap().remove(id);
    lease.closed.send_replace(true);
    let mut resource = lease.resource.lock().await;
    let result = if let Some(Resource::Remote(remote)) = resource.as_mut() {
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), remote.source.close())
            .await
            .map_err(|_| anyhow::anyhow!("stream cleanup timeout"))
            .and_then(|v| v);
        remote.finish(
            ContextState::cancelled("stream closed"),
            ContextStackPhase::Cancelled,
        );
        result
    } else {
        Ok(())
    };
    resource.take();
    result
}
pub fn transfer(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    authorized(ctx, id)?
        .transferred
        .store(true, std::sync::atomic::Ordering::Release);
    Ok(())
}
pub async fn close_untransferred(ctx: &Ctx) {
    close_owned(ctx, false).await;
}
pub async fn close_owned(ctx: &Ctx, include_transferred: bool) {
    let ids: Vec<String> = leases()
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, lease)| {
            lease.generation == ctx.get_engine().generation()
                && ctx.owns_scope(&lease.scope)
                && (include_transferred
                    || !lease.transferred.load(std::sync::atomic::Ordering::Acquire))
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        let _ = close(ctx, &id).await;
    }
}
pub fn model_contract() -> bridge::RuntimeContract {
    use async_openai::types::chat::CreateChatCompletionRequest;
    bridge::RuntimeContract {
        request: bridge::ValueCodec::serde::<CreateChatCompletionRequest>(),
        response: bridge::ValueCodec::new::<ModelResponse>(
            |ctx, value| match value.get("kind").and_then(Value::as_str) {
                Some("completed") => Ok(ModelResponse::Completed(serde_json::from_value(
                    value
                        .get("response")
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("missing model value"))?,
                )?)),
                Some("streaming") => {
                    let id = reference(&value)?.to_owned();
                    let lease = authorized(ctx, &id)?;
                    let remote = matches!(
                        lease
                            .resource
                            .try_lock()
                            .map_err(|_| anyhow::anyhow!("stream busy"))?
                            .as_ref(),
                        Some(Resource::Remote(_))
                    );
                    if remote {
                        let reader = Reader {
                            ctx: ctx.clone(),
                            id,
                        };
                        let stream =
                            futures_util::stream::unfold(Some(reader), |state| async move {
                                let reader = state?;
                                match next(&reader.ctx, &reader.id).await {
                                    Ok(value) if value["done"] == true => None,
                                    Ok(value) => Some((
                                        serde_json::from_value(value["value"].clone()).map_err(
                                            |e| {
                                                async_openai::error::OpenAIError::JSONDeserialize(
                                                    e,
                                                    String::new(),
                                                )
                                            },
                                        ),
                                        Some(reader),
                                    )),
                                    Err(error) => Some((
                                        Err(async_openai::error::OpenAIError::StreamError(
                                            Box::new(
                                                async_openai::error::StreamError::EventStream(
                                                    error.to_string(),
                                                ),
                                            ),
                                        )),
                                        None,
                                    )),
                                }
                            });
                        Ok(ModelResponse::Streaming(Box::pin(stream)))
                    } else {
                        match take(ctx, &id, true)? {
                            Resource::Model(model) => Ok(model),
                            _ => unreachable!(),
                        }
                    }
                }
                _ => anyhow::bail!("invalid model response"),
            },
            |ctx, model| match model {
                ModelResponse::Completed(value) => Ok(json!({"kind":"completed","response":value})),
                model => {
                    Ok(json!({"kind":"streaming","stream_id":insert(ctx, Resource::Model(model))?}))
                }
            },
        ),
    }
}
fn reference(value: &Value) -> anyhow::Result<&str> {
    value
        .get("stream_id")
        .or_else(|| value.get("reference"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing stream reference"))
}
pub fn tool_contract() -> bridge::RuntimeContract {
    bridge::RuntimeContract {
        request: bridge::ValueCodec::new::<ToolRequest>(
            |ctx, value| {
                let mut request: ToolRequest = serde_json::from_value(value.clone())?;
                if let Some(id) = value.get("invocation_ref").and_then(Value::as_str) {
                    let lease = authorized(ctx, id)?;
                    let resource = lease
                        .resource
                        .try_lock()
                        .map_err(|_| anyhow::anyhow!("resource busy"))?;
                    let Some(Resource::Invocation(invocation)) = resource.as_ref() else {
                        anyhow::bail!("resource type mismatch");
                    };
                    request = request.with_invocation(invocation.clone());
                }
                Ok(request)
            },
            |ctx, mut request| {
                let invocation = request.take_invocation();
                let mut value = serde_json::to_value(request)?;
                if let Some(invocation) = invocation {
                    value["invocation_ref"] =
                        Value::String(insert(ctx, Resource::Invocation(invocation))?);
                }
                Ok(value)
            },
        ),
        response: bridge::ValueCodec::new::<ToolResponse>(
            |ctx, value| {
                if value.get("stream_id").is_some() || value.get("reference").is_some() {
                    let id = reference(&value)?.to_owned();
                    let lease = authorized(ctx, &id)?;
                    let remote = matches!(
                        lease
                            .resource
                            .try_lock()
                            .map_err(|_| anyhow::anyhow!("stream busy"))?
                            .as_ref(),
                        Some(Resource::Remote(_))
                    );
                    if remote {
                        let reader = Reader {
                            ctx: ctx.clone(),
                            id,
                        };
                        let stream =
                            futures_util::stream::unfold(Some(reader), |state| async move {
                                let reader = state?;
                                match next(&reader.ctx, &reader.id).await {
                                    Ok(value) => {
                                        let item = serde_json::from_value(value["value"].clone())
                                            .map_err(Into::into);
                                        let next = if value["done"] == true {
                                            None
                                        } else {
                                            Some(reader)
                                        };
                                        Some((item, next))
                                    }
                                    Err(error) => Some((Err(error), None)),
                                }
                            });
                        return Ok(ToolResponse::with_stream(stream));
                    }
                    match take(ctx, reference(&value)?, false)? {
                        Resource::Tool(tool) => Ok(tool),
                        _ => unreachable!(),
                    }
                } else {
                    Ok(ToolResponse::with_result(
                        value
                            .get("result")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow::anyhow!("missing tool result"))?
                            .into(),
                    ))
                }
            },
            |ctx, tool| {
                let result = tool.plugin_result().map(str::to_owned);
                let id = insert(ctx, Resource::Tool(tool))?;
                Ok(match result {
                    Some(value) => json!({"result":value,"reference":id}),
                    None => json!({"reference":id}),
                })
            },
        ),
    }
}
