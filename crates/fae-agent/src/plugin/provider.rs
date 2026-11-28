//! Native trait adapters for providers bound to one immutable worker snapshot.
use super::manager::PluginNode;
use crate::{protocol::*, *};
use serde_json::{Value, json};
use std::sync::Arc;
use wd_tools::channel::{Channel, Receiver, Sender};

#[derive(Debug, Clone)]
pub struct ProviderEndpoint {
    pub node: Arc<PluginNode>,
    pub id: String,
    pub member: String,
}
impl ProviderEndpoint {
    pub fn context(ctx: &Ctx) -> anyhow::Result<WorkerContextRef> {
        Ok(WorkerContextRef {
            context_id: ctx.scope()?.scope_id.clone(),
        })
    }
    pub async fn call(
        &self,
        ctx: &Ctx,
        operation: &str,
        payload: Value,
        binding: WorkerBinding,
    ) -> anyhow::Result<Value> {
        self.call_with_contexts(ctx, operation, payload, binding, Vec::new())
            .await
    }
    pub async fn call_with_contexts(
        &self,
        ctx: &Ctx,
        operation: &str,
        payload: Value,
        binding: WorkerBinding,
        read_contexts: Vec<Ctx>,
    ) -> anyhow::Result<Value> {
        let request = self.request(ctx, operation, payload, binding)?;
        self.node
            .executor
            .provider_with_contexts(ctx, request, read_contexts)
            .await
    }

    fn request(
        &self,
        ctx: &Ctx,
        operation: &str,
        payload: Value,
        mut binding: WorkerBinding,
    ) -> anyhow::Result<WorkerRequestMessage> {
        let request_id = RequestId::new(wd_tools::uuid::v4()).map_err(anyhow::Error::from)?;
        binding.access = Some(PluginAccess {
            call_id: request_id.clone(),
            member: self.member.clone(),
            binding: plugin::codec::context(ctx, &self.member).binding,
        });
        let scope = ctx.scope()?;
        let plan = operation.starts_with("plan/").then(|| PlanRef {
            plan_id: self.id.clone(),
        });
        let call = operation.starts_with("runtime/").then(|| RuntimeCallRef {
            runtime_id: self.id.clone(),
        });
        Ok(WorkerRequestMessage {
            api_version: protocol::API_VERSION,
            request_id,
            event: format!("/request/plugin/{operation}"),
            payload,
            binding,
            plan,
            call,
            session: scope.execution.as_ref().map(|e| SessionRef {
                session_id: e.session.key().session_id,
            }),
        })
    }
}

struct PlanReleaseGuard {
    defer: Option<Box<dyn FnOnce() + Send + Sync>>,
    released: Arc<std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for PlanReleaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanReleaseGuard").finish_non_exhaustive()
    }
}

impl Drop for PlanReleaseGuard {
    fn drop(&mut self) {
        if !self.released.load(std::sync::atomic::Ordering::Acquire)
            && let Some(defer) = self.defer.take()
        {
            defer();
        }
    }
}

#[derive(Debug)]
pub struct PluginPlanBuilder {
    pub endpoint: ProviderEndpoint,
}
#[async_trait::async_trait]
impl PlanBuilder for PluginPlanBuilder {
    fn ty(&self) -> String {
        format!("plugin:plan:{}", self.endpoint.id)
    }
    async fn build(&self, _rt: RT, ctx: Ctx, env: AnyType) -> anyhow::Result<Box<dyn Plan>> {
        let env = *env
            .downcast::<Value>()
            .map_err(|_| anyhow::anyhow!("plugin Plan env must be JSON"))?;
        let instance_id = wd_tools::uuid::v4();
        let binding = WorkerBinding {
            generation: Some(ctx.get_engine().generation().into()),
            context: Some(ProviderEndpoint::context(&ctx)?),
            instance_id: Some(instance_id.clone()),
            ..Default::default()
        };
        let endpoint = self.endpoint.clone();
        let cleanup_id = instance_id.clone();
        let deferred_request = endpoint.request(
            &ctx,
            "plan/release",
            Value::Null,
            WorkerBinding {
                instance_id: Some(instance_id.clone()),
                ..Default::default()
            },
        )?;
        let defer = endpoint
            .node
            .executor
            .reserve_plan_release(deferred_request)?;
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cleanup_released = released.clone();
        let weak_ctx = ctx.downgrade();
        ctx.register_cleanup(ContextCleanup(Box::pin(async move {
            let ctx = weak_ctx
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("Plan Context expired before release"))?;
            let result = endpoint
                .call(
                    &ctx,
                    "plan/release",
                    Value::Null,
                    WorkerBinding {
                        instance_id: Some(cleanup_id),
                        ..Default::default()
                    },
                )
                .await;
            if result.is_ok() {
                cleanup_released.store(true, std::sync::atomic::Ordering::Release);
            }
            crate::plugin::leases::close_untransferred(&ctx).await;
            result.map(|_| ())
        })))?;
        let release_guard = PlanReleaseGuard {
            defer: Some(defer),
            released,
        };
        let returned = tokio::select! {
            biased;
            _ = ctx.cancellation() => return Err(crate::Error::ContextAborted.into()),
            result = self.endpoint.call(&ctx, "plan/create", env, binding) => result?,
        };
        anyhow::ensure!(
            returned.as_str() == Some(&instance_id),
            "Plan instance binding mismatch"
        );
        Ok(Box::new(PluginPlanProxy {
            endpoint: self.endpoint.clone(),
            instance_id,
            ctx,
            task_ids: std::sync::Mutex::new(std::collections::HashSet::new()),
            _release_guard: release_guard,
        }))
    }
}
#[derive(Debug)]
pub struct PluginPlanProxy {
    endpoint: ProviderEndpoint,
    instance_id: String,
    ctx: Ctx,
    task_ids: std::sync::Mutex<std::collections::HashSet<String>>,
    _release_guard: PlanReleaseGuard,
}
impl PluginPlanProxy {
    async fn advance(&self, method: &str, payload: Value) -> anyhow::Result<PlanNext> {
        self.advance_with_contexts(method, payload, Vec::new())
            .await
    }
    async fn advance_with_contexts(
        &self,
        method: &str,
        payload: Value,
        read_contexts: Vec<Ctx>,
    ) -> anyhow::Result<PlanNext> {
        let value = self
            .endpoint
            .call_with_contexts(
                &self.ctx,
                method,
                payload,
                WorkerBinding {
                    instance_id: Some(self.instance_id.clone()),
                    ..Default::default()
                },
                read_contexts,
            )
            .await?;
        match serde_json::from_value::<WorkerPlanNext>(value)? {
            WorkerPlanNext::End => Ok(PlanNext::End),
            WorkerPlanNext::Tasks { tasks } => {
                let context = ProviderEndpoint::context(&self.ctx)?;
                let mut requests = Vec::with_capacity(tasks.len());
                anyhow::ensure!(tasks.len() <= 256, "too many Plan tasks");
                for task in tasks {
                    anyhow::ensure!(
                        self.task_ids.lock().unwrap().insert(task.meta.id.clone()),
                        "Plan task ID was already used in this run"
                    );
                    anyhow::ensure!(
                        task.context.as_ref().is_none_or(|value| value == &context),
                        "Plan task Context is not owned"
                    );
                    let mut meta = task.meta.into_meta();
                    anyhow::ensure!(
                        meta.publisher.is_empty()
                            && meta.plan_run_id.is_empty()
                            && meta
                                .origin_plan_id
                                .as_deref()
                                .is_none_or(|id| id == self.endpoint.id),
                        "Plan task contains host-owned metadata"
                    );
                    meta.origin_plan_id = Some(self.endpoint.id.clone());
                    if meta.ty == TaskType::Plan {
                        anyhow::ensure!(
                            meta.target_plan_id.is_some(),
                            "Plan task requires target_plan_id"
                        );
                    } else {
                        anyhow::ensure!(
                            meta.target_plan_id.is_none(),
                            "target_plan_id requires a Plan task"
                        );
                    }
                    requests.push(
                        TaskReq {
                            ctx: self.ctx.clone(),
                            meta,
                            req: task.payload,
                        }
                        .into_request(),
                    );
                }
                Ok(PlanNext::Tasks(requests))
            }
        }
    }
}
#[async_trait::async_trait]
impl Plan for PluginPlanProxy {
    fn id(&self) -> &str {
        &self.endpoint.id
    }
    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        self.advance("plan/init", Value::Null).await
    }
    async fn next(&mut self, mut response: TaskResponse) -> anyhow::Result<PlanNext> {
        if response.payload().is::<()>() {
            response.replace_payload(Box::new(Value::Null));
        }
        if let Some(text) = response.payload().downcast_ref::<String>() {
            response.replace_payload(Box::new(Value::String(text.clone())));
        }
        let response = TaskResp::<Value>::try_from_response(&mut response)
            .ok_or_else(|| anyhow::anyhow!("Plan task response requires a JSON bridge"))?;
        self.advance_with_contexts(
            "plan/next",
            json!({
                "meta": WorkerTaskMeta::from(&response.meta),
                "context": ProviderEndpoint::context(&response.ctx)?, "payload": response.resp,
            }),
            if response.ctx.same_context(&self.ctx) {
                Vec::new()
            } else {
                vec![response.ctx]
            },
        )
        .await
    }
    async fn abort(&mut self, code: i32, error: String) {
        let _ = self
            .endpoint
            .call(
                &self.ctx,
                "plan/abort",
                Value::Null,
                WorkerBinding {
                    instance_id: Some(self.instance_id.clone()),
                    code: Some(code),
                    error: Some(error),
                    ..Default::default()
                },
            )
            .await;
    }
}

#[derive(Debug, Clone)]
pub struct PluginRuntimeProxy {
    generation: String,
    pub endpoint: ProviderEndpoint,
    pub task_types: Vec<TaskType>,
    sender: Sender<Event>,
    receiver: Receiver<Event>,
    tasks: Arc<std::sync::Mutex<std::collections::HashMap<String, Ctx>>>,
    exited: Arc<std::sync::atomic::AtomicBool>,
}
impl PluginRuntimeProxy {
    pub fn new(endpoint: ProviderEndpoint, task_types: Vec<TaskType>, generation: String) -> Self {
        let (sender, receiver) = Channel::new(256);
        Self {
            generation,
            endpoint,
            task_types,
            sender,
            receiver,
            tasks: Arc::default(),
            exited: Arc::default(),
        }
    }
}
#[async_trait::async_trait]
impl RuntimeSelectExec<Value, Value, Value, Value> for PluginRuntimeProxy {
    fn id(&self) -> &str {
        &self.endpoint.id
    }
    fn tys(&self) -> Vec<TaskType> {
        self.task_types.clone()
    }
    async fn watch(&self) -> crate::Result<Receiver<Event>> {
        Ok(self.receiver.clone())
    }
    async fn exec(&self, task: TaskReq<Value>) -> crate::Result<TaskResp<Value>> {
        if self.exited.load(std::sync::atomic::Ordering::Acquire) {
            return Err(anyhow::anyhow!("Runtime is shutting down").into());
        }
        let key = format!("{}:{}", task.ctx.scope()?.scope_id, task.meta.id);
        {
            let mut tasks = self.tasks.lock().unwrap();
            if tasks.len() >= 256 {
                return Err(anyhow::anyhow!("Runtime active task capacity exceeded").into());
            }
            if tasks.contains_key(&key) {
                return Err(anyhow::anyhow!("duplicate active Runtime task").into());
            }
            tasks.insert(key.clone(), task.ctx.clone());
        }
        let _lease = RuntimeTaskLease {
            tasks: self.tasks.clone(),
            key,
            ctx: task.ctx.clone(),
        };
        let call = self.endpoint.call(
            &task.ctx,
            "runtime/exec",
            task.req,
            WorkerBinding {
                generation: Some(task.ctx.get_engine().generation().into()),
                context: Some(ProviderEndpoint::context(&task.ctx)?),
                meta: Some(WorkerTaskMeta::from(&task.meta)),
                ..Default::default()
            },
        );
        let result = tokio::select! {
            result = call => result,
            _ = task.ctx.cancellation() => {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2),
                    self.endpoint.call(&task.ctx, "runtime/kill", Value::Null, WorkerBinding {
                        generation: Some(self.generation.clone()),
                        context: Some(ProviderEndpoint::context(&task.ctx)?),
                        task_id: Some(task.meta.id.clone()), ..Default::default()
                    })).await;
                return Err(crate::Error::ContextAborted);
            }
        };
        let mut result = result?;
        if !crate::plugin::leases::has_remote(&task.ctx) {
            let contract = match task.meta.ty {
                TaskType::Model => Some(crate::plugin::leases::model_contract()),
                TaskType::Tool => Some(crate::plugin::leases::tool_contract()),
                _ => None,
            };
            if let Some(contract) = contract {
                let payload = contract.response.decode(&task.ctx, result)?;
                let mut response = TaskResp {
                    ctx: task.ctx.clone(),
                    meta: task.meta.clone(),
                    resp: (),
                }
                .into_response();
                response.replace_payload(payload);
                response.track_completion();
                result = contract
                    .response
                    .encode(&task.ctx, response.take_payload())?;
            }
        }
        Ok(TaskResp {
            ctx: task.ctx,
            meta: task.meta,
            resp: result,
        })
    }
    async fn spawn(&self, task: TaskReq<Value>) -> crate::Result<()> {
        let runtime = self.clone();
        tokio::spawn(async move {
            let ctx = task.ctx.clone();
            let meta = task.meta.clone();
            let event_type = match runtime.exec(task).await {
                Ok(response) => EventType::TaskResult(response.into_response()),
                Err(error) => {
                    let _ = ctx.set_state(ContextState::failed(error.to_string()));
                    let _ = ctx.invocation_finished();
                    EventType::TaskError(TaskError {
                        ctx,
                        meta,
                        error: error.to_string(),
                    })
                }
            };
            let _ = runtime
                .sender
                .send(Event {
                    from_rt_id: runtime.endpoint.id.clone(),
                    event_type,
                })
                .await;
        });
        Ok(())
    }
    async fn select_with_context(
        &self,
        ctx: &Ctx,
        ty: TaskType,
        cond: Value,
    ) -> crate::Result<Value> {
        if self.exited.load(std::sync::atomic::Ordering::Acquire) {
            return Err(anyhow::anyhow!("Runtime is shutting down").into());
        }
        Ok(self
            .endpoint
            .call(
                ctx,
                "runtime/select",
                cond,
                WorkerBinding {
                    generation: Some(ctx.get_engine().generation().into()),
                    context: Some(ProviderEndpoint::context(ctx)?),
                    ty: Some(ty),
                    ..Default::default()
                },
            )
            .await?)
    }
    async fn kill(&self, task_id: &str) -> crate::Result<()> {
        let contexts: Vec<_> = self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .filter(|(key, _)| key.ends_with(&format!(":{task_id}")))
            .map(|(_, ctx)| ctx.clone())
            .collect();
        if contexts.len() > 1 {
            return Err(anyhow::anyhow!(
                "Conflict: task_id is ambiguous; use kill_on with Context"
            )
            .into());
        }
        for ctx in contexts {
            self.kill_with_context(&ctx, task_id).await?;
        }
        Ok(())
    }
    async fn kill_with_context(&self, ctx: &Ctx, task_id: &str) -> crate::Result<()> {
        let key = format!("{}:{task_id}", ctx.scope()?.scope_id);
        let active = self.tasks.lock().unwrap().contains_key(&key);
        if active {
            self.endpoint
                .call(
                    &ctx,
                    "runtime/kill",
                    Value::Null,
                    WorkerBinding {
                        generation: Some(self.generation.clone()),
                        context: Some(ProviderEndpoint::context(&ctx)?),
                        task_id: Some(task_id.into()),
                        ..Default::default()
                    },
                )
                .await?;
        }
        Ok(())
    }
    async fn exit(&self) -> crate::Result<()> {
        if self.exited.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Ok(());
        }
        let contexts: Vec<_> = self.tasks.lock().unwrap().values().cloned().collect();
        for ctx in contexts {
            let _ = ctx.request_cancel("Runtime is shutting down".into());
            crate::plugin::leases::close_owned(&ctx, true).await;
        }
        let request_id = RequestId::new(wd_tools::uuid::v4()).map_err(anyhow::Error::from)?;
        let request = WorkerRequestMessage {
            api_version: protocol::API_VERSION,
            request_id: request_id.clone(),
            event: "/request/plugin/runtime/exit".into(),
            session: None,
            plan: None,
            call: Some(RuntimeCallRef {
                runtime_id: self.endpoint.id.clone(),
            }),
            payload: Value::Null,
            binding: WorkerBinding {
                generation: Some(self.generation.clone()),
                access: Some(PluginAccess {
                    call_id: request_id,
                    member: self.endpoint.member.clone(),
                    binding: PluginCapability {
                        handle: self.endpoint.member.clone(),
                        execution_id: None,
                        invocation_id: None,
                    },
                }),
                ..Default::default()
            },
        };
        self.endpoint
            .node
            .executor
            .provider_control(request)
            .await?;
        Ok(())
    }
}
struct RuntimeTaskLease {
    tasks: Arc<std::sync::Mutex<std::collections::HashMap<String, Ctx>>>,
    key: String,
    ctx: Ctx,
}
impl Drop for RuntimeTaskLease {
    fn drop(&mut self) {
        if crate::plugin::leases::has_resources(&self.ctx)
            && !self.ctx.state().current().is_terminal()
        {
            let tasks = self.tasks.clone();
            let key = self.key.clone();
            let ctx = self.ctx.clone();
            tokio::spawn(async move {
                let _ = ctx.terminal().await;
                tasks.lock().unwrap().remove(&key);
            });
            return;
        }
        self.tasks.lock().unwrap().remove(&self.key);
    }
}
