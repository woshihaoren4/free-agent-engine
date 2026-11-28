use super::*;
use crate::protocol::WorkerRequestMessage;

pub(super) struct ProviderCall {
    worker: Weak<Worker>,
    request: WorkerRequestMessage,
    ctx: Ctx,
    deadline: Instant,
    active: AtomicBool,
    children: Mutex<Vec<Ctx>>,
    read_contexts: Vec<Ctx>,
}
struct ProviderLease {
    worker: Arc<Worker>,
    call: Arc<ProviderCall>,
}
impl Drop for ProviderLease {
    fn drop(&mut self) {
        self.call.active.store(false, Ordering::SeqCst);
        for child in self.call.children.lock().unwrap().iter() {
            if !child.state().current().is_terminal()
                && !crate::plugin::leases::has_resources(child)
            {
                let _ = child.request_cancel("provider capability expired".into());
            }
        }
        self.worker
            .providers
            .lock()
            .unwrap()
            .remove(self.call.request.request_id.as_str());
    }
}

impl PythonExecutor {
    pub(super) async fn invoke_provider(
        &self,
        ctx: &Ctx,
        request: WorkerRequestMessage,
    ) -> anyhow::Result<Value> {
        self.invoke_provider_with_contexts(ctx, request, Vec::new())
            .await
    }
    pub(super) async fn invoke_provider_with_contexts(
        &self,
        ctx: &Ctx,
        request: WorkerRequestMessage,
        read_contexts: Vec<Ctx>,
    ) -> anyhow::Result<Value> {
        request.validate()?;
        anyhow::ensure!(
            self.worker.healthy.load(Ordering::SeqCst),
            "plugin_worker_unavailable"
        );
        anyhow::ensure!(
            request
                .binding
                .generation
                .as_deref()
                .is_none_or(|generation| generation == ctx.get_engine().generation()),
            "provider generation mismatch"
        );
        let id = request.request_id.to_string();
        for child in &read_contexts {
            let scope = child.scope()?;
            anyhow::ensure!(
                ctx.owns_scope(&scope.scope_id)
                    && child.get_engine().generation() == ctx.get_engine().generation(),
                "invalid child Context grant"
            );
        }
        let call = Arc::new(ProviderCall {
            worker: Arc::downgrade(&self.worker),
            request: request.clone(),
            ctx: ctx.clone(),
            deadline: Instant::now() + Duration::from_secs(120),
            active: AtomicBool::new(true),
            children: Mutex::new(Vec::new()),
            read_contexts,
        });
        let (send, receive) = oneshot::channel();
        {
            let mut pending = self.worker.pending.lock().unwrap();
            anyhow::ensure!(pending.len() < 256, "plugin_busy: requests");
            pending.insert(
                id.clone(),
                (PendingRequest::Provider(request.clone()), send),
            );
        }
        self.worker
            .providers
            .lock()
            .unwrap()
            .insert(id.clone(), Arc::downgrade(&call));
        let _lease = ProviderLease {
            worker: self.worker.clone(),
            call: call.clone(),
        };
        let _pending = Pending {
            worker: &self.worker,
            id,
        };
        tokio::time::timeout_at(call.deadline, async {
            self.worker
                .write
                .send(serde_json::to_value(request)?)
                .await?;
            receive.await?
        })
        .await
        .map_err(|_| anyhow::anyhow!("plugin_call_timeout"))?
    }
}

impl Worker {
    pub(super) fn dispatch_provider(self: &Arc<Self>, frame: Value) -> anyhow::Result<()> {
        let request: WorkerRequestMessage = serde_json::from_value(frame)?;
        let access = request
            .binding
            .access
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
        let call = self
            .providers
            .lock()
            .unwrap()
            .get(access.call_id.as_str())
            .and_then(Weak::upgrade);
        let permit = self.host_slots.clone().try_acquire_owned();
        let sender = self.control.clone();
        tokio::spawn(async move {
            let result = match (call, permit) {
                (Some(call), Ok(_permit)) => {
                    tokio::time::timeout_at(call.deadline, call.host(&request))
                        .await
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("HostCallTimeout")))
                }
                (None, _) => Err(anyhow::anyhow!("HandleExpired")),
                _ => Err(anyhow::anyhow!("plugin_busy: host_requests")),
            };
            let (code, msg, payload) = match result {
                Ok(value) => (ResponseCode::Success, "ok".into(), value),
                Err(error) => (error_code(&error), error.to_string(), Value::Null),
            };
            let reply = ResponseMessage {
                api_version: protocol::API_VERSION,
                response_id: request.request_id.clone(),
                session: request.session.clone(),
                plan: request.plan.clone(),
                code,
                msg,
                payload,
            };
            let _ = sender.send(serde_json::to_value(reply).unwrap()).await;
        });
        Ok(())
    }
}
impl ProviderCall {
    async fn host(&self, request: &WorkerRequestMessage) -> anyhow::Result<Value> {
        anyhow::ensure!(self.active.load(Ordering::SeqCst), "HandleExpired");
        anyhow::ensure!(
            serde_json::to_value(&request.binding.access)?
                == serde_json::to_value(&self.request.binding.access)?
                && request.session == self.request.session,
            "HandleExpired"
        );
        if matches!(
            request.event.as_str(),
            "/request/plugin/host/stream/next" | "/request/plugin/host/stream/close"
        ) {
            let id = request
                .payload
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid stream reference"))?;
            return if request.event.ends_with("/next") {
                crate::plugin::leases::next(&self.ctx, id).await
            } else {
                crate::plugin::leases::close(&self.ctx, id).await?;
                Ok(Value::Null)
            };
        }
        anyhow::ensure!(
            !self.ctx.cancel_requested() && !self.ctx.state().current().is_terminal(),
            "ContextFinished"
        );
        let context = if let Some(reference) = &request.binding.context {
            if reference.context_id == self.ctx.scope()?.scope_id {
                &self.ctx
            } else {
                anyhow::ensure!(
                    request.event == "/request/plugin/context/get",
                    "Context is read-only"
                );
                self.read_contexts
                    .iter()
                    .find(|ctx| {
                        ctx.scope()
                            .is_ok_and(|s| s.scope_id == reference.context_id)
                    })
                    .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?
            }
        } else {
            &self.ctx
        };
        match request.event.as_str() {
            "/request/plugin/host/stream/register" => {
                let id = request
                    .payload
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid worker stream"))?;
                let mut meta = self
                    .request
                    .binding
                    .meta
                    .as_ref()
                    .map(|m| m.clone().into_meta())
                    .unwrap_or_else(|| TaskMeta {
                        id: wd_tools::uuid::v4(),
                        ..Default::default()
                    });
                let source = Arc::new(ProviderStream {
                    worker: self
                        .worker
                        .upgrade()
                        .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?,
                    ctx: self.ctx.clone(),
                    member: self.request.binding.access.as_ref().unwrap().member.clone(),
                    id: id.into(),
                });
                let stream_ctx = if self.request.plan.is_some() {
                    let ctx = self.ctx.fork(ContextChildSpec::default())?;
                    meta.ty = TaskType::Any("boundary_stream".into());
                    ctx.invocation_started(&meta)?;
                    ctx.set_state(ContextState::Running)?;
                    ctx
                } else {
                    self.ctx.clone()
                };
                let id = crate::plugin::leases::register_stream(&stream_ctx, meta, source)?;
                Ok(json!({"kind":"streaming","stream_id":id}))
            }
            "/request/plugin/context/complete" => {
                if let Some(resources) = &request.binding.resources {
                    anyhow::ensure!(resources.len() <= 16, "too many output resources");
                    for id in resources {
                        crate::plugin::leases::transfer(&self.ctx, id)?;
                    }
                }
                self.ctx
                    .submit_output(ContextOutput::new(Box::new(request.payload.clone())))?;
                Ok(Value::Null)
            }
            "/request/plugin/context/get" => {
                anyhow::ensure!(
                    request
                        .binding
                        .query
                        .as_ref()
                        .and_then(|query| query.limit)
                        .is_none_or(|limit| limit <= 256),
                    "invalid stack limit"
                );
                let scope = context.scope()?;
                Ok(json!({
                    "context_id": scope.scope_id, "user_id": scope.user.user_id,
                    "stacks": context.stacks(request.binding.query.clone().unwrap_or_default())?,
                    "session": scope.execution.as_ref().map(|e| e.session.read_summary()),
                }))
            }
            "/request/plugin/host/runtime/exec" => {
                let mut meta = request.binding.meta.clone().unwrap().into_meta();
                anyhow::ensure!(
                    meta.publisher.is_empty() && meta.plan_run_id.is_empty(),
                    "host-owned task metadata"
                );
                let origin = self.request.plan.as_ref().map(|plan| plan.plan_id.clone());
                anyhow::ensure!(
                    meta.origin_plan_id
                        .as_ref()
                        .is_none_or(|id| Some(id) == origin.as_ref()),
                    "task origin mismatch"
                );
                meta.origin_plan_id = origin;
                let child = self.ctx.fork(ContextChildSpec::default())?;
                self.children.lock().unwrap().push(child.clone());
                let mut task = TaskReq {
                    ctx: child,
                    meta,
                    req: request.payload.clone(),
                }
                .into_request();
                let mut response = Runtime::exec(&*self.ctx.get_engine().rt(), &mut task).await?;
                let response = TaskResp::<Value>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("runtime response requires a JSON bridge"))?;
                Ok(response.resp)
            }
            "/request/plugin/host/runtime/select" => {
                let value = self
                    .ctx
                    .get_engine()
                    .select_on(
                        &self.ctx,
                        RuntimeCallMeta {
                            runtime_id: request.call.as_ref().unwrap().runtime_id.clone(),
                            ty: request.binding.ty.clone().unwrap(),
                        },
                        Box::new(request.payload.clone()),
                    )
                    .await?;
                value
                    .downcast::<Value>()
                    .map(|v| *v)
                    .map_err(|_| anyhow::anyhow!("runtime select requires a JSON bridge"))
            }
            "/request/plugin/host/plan/call" => {
                let (child, plan) = self
                    .ctx
                    .get_engine()
                    .fork_plan(
                        &self.ctx,
                        PlanCallMeta {
                            plan_id: request.plan.as_ref().unwrap().plan_id.clone(),
                        },
                        Box::new(request.payload.clone()),
                    )
                    .await?;
                self.children.lock().unwrap().push(child.clone());
                let mut task = TaskReq {
                    ctx: child.clone(),
                    meta: TaskMeta {
                        id: wd_tools::uuid::v4(),
                        ty: TaskType::Plan,
                        ..Default::default()
                    },
                    req: plan,
                }
                .into_request();
                Runtime::exec(&*self.ctx.get_engine().rt(), &mut task).await?;
                child.result::<Value>().await
            }
            _ => anyhow::bail!("unsupported worker capability"),
        }
    }
}

struct ProviderStream {
    worker: Arc<Worker>,
    ctx: Ctx,
    member: String,
    id: String,
}
impl ProviderStream {
    async fn call(&self, operation: &str) -> anyhow::Result<Value> {
        let request_id = crate::RequestId::new(wd_tools::uuid::v4())?;
        let request = WorkerRequestMessage {
            api_version: protocol::API_VERSION,
            request_id: request_id.clone(),
            event: format!("/request/plugin/resource/{operation}"),
            payload: Value::String(self.id.clone()),
            session: self
                .ctx
                .scope()?
                .execution
                .as_ref()
                .map(|e| protocol::SessionRef {
                    session_id: e.session.key().session_id,
                }),
            plan: None,
            call: None,
            binding: protocol::WorkerBinding {
                generation: Some(self.ctx.get_engine().generation().into()),
                context: Some(protocol::WorkerContextRef {
                    context_id: self.ctx.scope()?.scope_id.clone(),
                }),
                access: Some(protocol::PluginAccess {
                    call_id: request_id,
                    member: self.member.clone(),
                    binding: crate::plugin::codec::context(&self.ctx, &self.member).binding,
                }),
                ..Default::default()
            },
        };
        let executor = PythonExecutor {
            worker: self.worker.clone(),
            members: Vec::new(),
        };
        executor.invoke_provider(&self.ctx, request).await
    }
}
#[async_trait::async_trait]
impl crate::plugin::leases::BoundaryStream for ProviderStream {
    async fn next(&self) -> anyhow::Result<Value> {
        self.call("next").await
    }
    async fn close(&self) -> anyhow::Result<()> {
        self.call("close").await.map(|_| ())
    }
}
