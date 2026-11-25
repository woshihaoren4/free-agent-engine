//! Framed multiplexed Python bridge. The read loop never waits for business work.
use crate::protocol::{Message, RequestPath, RequestPayload, ResponseCode, ResponseMessage};
use crate::{
    plugin::{manager::PluginRecord, *},
    *,
};
use async_openai::{error::OpenAIError, types::chat::ChatCompletionResponseStream};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{mpsc, oneshot},
    time::{Instant, timeout},
};

const MAX_FRAME: usize = 4 * 1024 * 1024;
type Reply = oneshot::Sender<anyhow::Result<Value>>;
type PendingReply = (Message, Reply);
struct Manifest(PathBuf);
impl Drop for Manifest {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn command(module: &str) -> Command {
    let mut cmd =
        Command::new(std::env::var_os("FAE_PLUGIN_PYTHON").unwrap_or_else(|| "python3".into()));
    cmd.args(["-u", "-B", "-m", module])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    cmd
}
async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> anyhow::Result<Value> {
    let size = reader.read_u32().await? as usize;
    anyhow::ensure!(size > 0 && size <= MAX_FRAME, "invalid plugin frame length");
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn request(
    id: &str,
    path: RequestPath,
    session_id: Option<SessionId>,
    payload: Value,
) -> anyhow::Result<Message> {
    Ok(serde_json::from_value(json!({
        "version": protocol::VERSION, "request_id": id, "session_id": session_id,
        "path": path, "payload": payload
    }))?)
}
async fn write_frame(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &impl serde::Serialize,
) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    anyhow::ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_FRAME,
        "invalid plugin frame length"
    );
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
pub async fn inspect(id: &str, source: &Path) -> anyhow::Result<PluginDeclaration> {
    if source.is_dir() {
        let d: PluginDeclaration =
            serde_json::from_slice(&tokio::fs::read(source.join("plugin.config")).await?)?;
        let d = d.normalize(id, None)?;
        anyhow::ensure!(
            d.engine == Some(PluginEngine::Py),
            "plugin_engine_unavailable: js"
        );
        return Ok(d);
    }
    anyhow::ensure!(
        source.extension().and_then(|s| s.to_str()) == Some("py"),
        "plugin_engine_unavailable"
    );
    let mut child = command("fae_plugin._inspect").arg(id).arg(source).spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let message = request(
        &wd_tools::uuid::v4(),
        RequestPath::PluginInspect,
        None,
        json!({"id":id,"source":source.to_string_lossy()}),
    )?;
    write_frame(child.stdin.as_mut().unwrap(), &message).await?;
    let response: ResponseMessage =
        serde_json::from_value(timeout(Duration::from_secs(5), read_frame(&mut stdout)).await??)?;
    let status = timeout(Duration::from_secs(5), child.wait()).await??;
    anyhow::ensure!(
        status.success()
            && response.request_id == message.request_id.clone().unwrap()
            && response.path == message.as_request()?.path().response()
            && response.payload.code == ResponseCode::Success,
        "plugin_config_invalid: {}",
        response.payload.msg
    );
    Ok(
        serde_json::from_value::<PluginDeclaration>(serde_json::to_value(response.payload.data)?)?
            .normalize(id, Some(PluginEngine::Py))?,
    )
}

#[derive(Debug)]
pub struct PythonExecutor {
    worker: Arc<Worker>,
    members: Vec<PluginRecord>,
}
struct Worker {
    _child: tokio::sync::Mutex<Child>,
    write: mpsc::Sender<Value>,
    control: mpsc::Sender<Value>,
    pending: Mutex<HashMap<String, PendingReply>>,
    calls: Mutex<HashMap<String, Weak<PluginCall>>>,
    host_slots: Arc<tokio::sync::Semaphore>,
    healthy: AtomicBool,
    failed: tokio::sync::watch::Sender<bool>,
    last_error: Mutex<Option<String>>,
}
struct Pending<'a> {
    worker: &'a Worker,
    id: String,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.worker.pending.lock().unwrap().remove(&self.id);
    }
}
impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PythonWorker")
    }
}
struct PluginCall {
    id: String,
    worker: Weak<Worker>,
    ctx: Ctx,
    root: Ctx,
    metadata: Value,
    point: String,
    method: String,
    next_members: Mutex<std::collections::HashSet<String>>,
    deadline: Instant,
    members: HashMap<String, (String, String)>,
    contexts: Mutex<HashMap<String, Ctx>>,
    streams: tokio::sync::Mutex<HashMap<String, ChatCompletionResponseStream>>,
    tool: Mutex<Option<ToolResponse>>,
    receipts: tokio::sync::Mutex<HashMap<String, Value>>,
    query_completion: Mutex<Option<String>>,
    released: tokio::sync::watch::Sender<bool>,
    release_requested: AtomicBool,
    terminal_controls: AtomicUsize,
    streaming: AtomicBool,
}
// A state-changing RPC must enqueue its acknowledgement before invalidation,
// even if an outer runtime drops the invocation as soon as the state changes.
struct TerminalControl(Arc<PluginCall>);
impl Drop for TerminalControl {
    fn drop(&mut self) {
        if self.0.terminal_controls.fetch_sub(1, Ordering::SeqCst) == 1
            && self.0.release_requested.load(Ordering::SeqCst)
        {
            self.0.release();
        }
    }
}
struct Lease(Arc<PluginCall>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.release();
    }
}
impl PluginCall {
    fn release(&self) {
        self.release_requested.store(true, Ordering::SeqCst);
        if self.terminal_controls.load(Ordering::SeqCst) > 0 {
            return;
        }
        if *self.released.borrow() {
            return;
        }
        self.released.send_replace(true);
        for ctx in self.contexts.lock().unwrap().values() {
            if ctx
                .scope()
                .ok()
                .and_then(|s| s.parent_scope_id.clone())
                .is_some()
                && ctx.scope().ok().map(|s| s.scope_id.clone())
                    != self.ctx.scope().ok().map(|s| s.scope_id.clone())
                && !ctx.state().current().is_terminal()
            {
                let _ = ctx.set_state(ContextState::cancelled("plugin invocation released"));
            }
        }
    }
}
impl Worker {
    async fn rpc(
        &self,
        method: RequestPath,
        params: Value,
        deadline: Instant,
    ) -> anyhow::Result<Value> {
        let id = wd_tools::uuid::v4();
        self.rpc_id(id, method, params, deadline).await
    }
    async fn rpc_id(
        &self,
        id: String,
        method: RequestPath,
        params: Value,
        deadline: Instant,
    ) -> anyhow::Result<Value> {
        let (send, receive) = oneshot::channel();
        let session_id = params
            .get("contexts")
            .and_then(Value::as_object)
            .and_then(|c| c.values().next())
            .and_then(|c| c.get("session_id"))
            .filter(|s| !s.is_null())
            .map(|s| serde_json::from_value(s.clone()))
            .transpose()?
            .or_else(|| {
                params
                    .get("call_id")
                    .or_else(|| params.get("access")?.get("call_id"))
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .and_then(|id| self.calls.lock().unwrap().get(id).and_then(Weak::upgrade))
                    .and_then(|call| call.ctx.scope().ok())
                    .and_then(|s| s.execution.as_ref().map(|e| e.session.key().session_id))
            });
        let message = request(&id, method, session_id, params)?;
        {
            let mut pending = self.pending.lock().unwrap();
            anyhow::ensure!(pending.len() < 256, "plugin_busy: requests");
            pending.insert(id.clone(), (message.clone(), send));
        }
        let _pending = Pending {
            worker: self,
            id: id.clone(),
        };
        let result = tokio::time::timeout_at(deadline, async {
            self.write.send(serde_json::to_value(message)?).await?;
            receive.await?
        })
        .await;
        let result = result.unwrap_or_else(|_| Err(anyhow::anyhow!("plugin_call_timeout")));
        if let Err(error) = &result {
            *self.last_error.lock().unwrap() = Some(error.to_string());
        }
        result
    }
    fn fail(&self, error: &str) {
        self.healthy.store(false, Ordering::SeqCst);
        *self.last_error.lock().unwrap() = Some(error.into());
        self.failed.send_replace(true);
        for (_, (_, reply)) in self.pending.lock().unwrap().drain() {
            let _ = reply.send(Err(anyhow::anyhow!("plugin_worker_unavailable: {error}")));
        }
    }
}
impl PythonExecutor {
    pub async fn start(members: Vec<PluginRecord>) -> anyhow::Result<Self> {
        let file = members[0]
            .source
            .parent()
            .unwrap()
            .join(format!(".worker-{}.json", wd_tools::uuid::v4()));
        let _manifest = Manifest(file.clone());
        tokio::fs::write(&file, serde_json::to_vec(&json!({"members":members}))?).await?;
        let spawn = command("fae_plugin._worker").arg(&file).spawn();
        let mut child = spawn?;
        let mut stdout = child.stdout.take().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let ready: Message = serde_json::from_value(
            timeout(Duration::from_secs(5), read_frame(&mut stdout)).await??,
        )?;
        let _ = tokio::fs::remove_file(&file).await;
        let RequestPayload::PluginWorkerHello(hello) = ready.as_request()? else {
            anyhow::bail!("plugin_startup_failed: expected worker hello");
        };
        let expected = members
            .iter()
            .flat_map(|r| (0..r.declaration.mounts.len()).map(move |i| format!("{}:{i}", r.id)))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            hello.api_version == API_VERSION
                && hello.instances == expected
                && ready.session_id.is_none(),
            "plugin_startup_failed: incompatible worker"
        );
        write_frame(
            &mut stdin,
            &ResponseMessage::success(
                &ready,
                None,
                "ready",
                Some(protocol::ResponsePayload::PluginWorkerHello(
                    protocol::Empty {},
                )),
            )?,
        )
        .await?;
        let (write, mut queue) = mpsc::channel::<Value>(128);
        let (control, mut controls) = mpsc::channel::<Value>(64);
        let worker = Arc::new(Worker {
            _child: tokio::sync::Mutex::new(child),
            write,
            control,
            pending: Mutex::new(HashMap::new()),
            calls: Mutex::new(HashMap::new()),
            host_slots: Arc::new(tokio::sync::Semaphore::new(256)),
            healthy: AtomicBool::new(true),
            failed: tokio::sync::watch::channel(false).0,
            last_error: Mutex::new(None),
        });
        let weak = Arc::downgrade(&worker);
        tokio::spawn(async move {
            loop {
                let frame = tokio::select! {
                    biased;
                    frame=controls.recv()=>frame,
                    frame=queue.recv()=>frame,
                };
                let Some(frame) = frame else { break };
                let bytes = match serde_json::to_vec(&frame) {
                    Ok(b) if b.len() <= MAX_FRAME => b,
                    _ => break,
                };
                if stdin.write_u32(bytes.len() as u32).await.is_err()
                    || stdin.write_all(&bytes).await.is_err()
                {
                    break;
                }
            }
            if let Some(worker) = weak.upgrade() {
                worker.fail("writer closed");
            }
        });
        let weak = Arc::downgrade(&worker);
        tokio::spawn(async move {
            loop {
                let frame = match read_frame(&mut stdout).await {
                    Ok(frame) => frame,
                    Err(error) => {
                        if let Some(w) = weak.upgrade() {
                            w.fail(&error.to_string());
                        }
                        break;
                    }
                };
                let Some(worker) = weak.upgrade() else {
                    break;
                };
                if frame["path"]
                    .as_str()
                    .is_some_and(|p| p.starts_with("/request/"))
                {
                    let message: Message = match serde_json::from_value(frame) {
                        Ok(message) => message,
                        Err(error) => {
                            worker.fail(&error.to_string());
                            break;
                        }
                    };
                    let call = worker.call_for(&message);
                    let permit = worker.host_slots.clone().try_acquire_owned();
                    let sender = worker.control.clone();
                    let control = call
                        .as_ref()
                        .filter(|_| {
                            matches!(
                                message.as_request().map(|p| p.path()).ok(),
                                Some(
                                    RequestPath::PluginContextCancel
                                        | RequestPath::SessionExecutionStop
                                )
                            )
                        })
                        .map(|call| {
                            call.terminal_controls.fetch_add(1, Ordering::SeqCst);
                            TerminalControl(call.clone())
                        });
                    tokio::spawn(async move {
                        let result = match (call, permit) {
                            (Some(call), Ok(_permit)) => {
                                let mut released = call.released.subscribe();
                                tokio::select! {
                                    r=tokio::time::timeout_at(call.deadline,call.host(&message))=>
                                        r.unwrap_or_else(|_|Err(anyhow::anyhow!("HostCallTimeout"))),
                                    _=released.changed()=>Err(anyhow::anyhow!("HandleExpired")),
                                }
                            }
                            (None, _) => Err(anyhow::anyhow!("HandleExpired")),
                            _ => Err(anyhow::anyhow!("plugin_busy: host_requests")),
                        };
                        let reply = match result {
                            Ok(result) => json!({"version":protocol::VERSION,
                                "request_id":message.request_id,"session_id":message.session_id,
                                "path":message.as_request().unwrap().path().response(),
                                "payload":{"code":0,"msg":"ok","data":result}}),
                            Err(error) => {
                                let text = error.to_string();
                                serde_json::to_value(
                                    ResponseMessage::error(&message, error_code(&error), text)
                                        .unwrap(),
                                )
                                .unwrap()
                            }
                        };
                        let reply = match serde_json::from_value::<ResponseMessage>(reply) {
                            Ok(response) => serde_json::to_value(response).unwrap(),
                            Err(error) => serde_json::to_value(
                                ResponseMessage::error(
                                    &message,
                                    ResponseCode::InternalError,
                                    format!("plugin response schema: {error}"),
                                )
                                .unwrap(),
                            )
                            .unwrap(),
                        };
                        let _ = sender.send(reply).await;
                        drop(control);
                    });
                } else {
                    let response: ResponseMessage = match serde_json::from_value(frame) {
                        Ok(response) => response,
                        Err(error) => {
                            worker.fail(&error.to_string());
                            break;
                        }
                    };
                    let pending = worker
                        .pending
                        .lock()
                        .unwrap()
                        .remove(response.request_id.as_str());
                    if let Some((request, reply)) = pending {
                        let result = if let Err(error) = response.validate_for(&request) {
                            Err(anyhow::anyhow!("plugin response binding mismatch: {error}"))
                        } else if response.payload.code != ResponseCode::Success {
                            Err(anyhow::anyhow!(
                                "Python plugin ({}): {}",
                                response.payload.code as u16,
                                response.payload.msg
                            ))
                        } else {
                            serde_json::to_value(response.payload.data).map_err(Into::into)
                        };
                        let _ = reply.send(result);
                    }
                }
            }
        });
        Ok(Self { worker, members })
    }
}
fn error_code(error: &anyhow::Error) -> ResponseCode {
    if let Some(error) = error.downcast_ref::<SessionControlError>() {
        return match error {
            SessionControlError::Conflict(_) => ResponseCode::Conflict,
            SessionControlError::NotFound(_) => ResponseCode::NotFound,
            SessionControlError::Invalid(_) => ResponseCode::BadRequest,
        };
    }
    let text = error.to_string();
    for (hint, code) in [
        ("HandleExpired", ResponseCode::NotFound),
        ("terminal", ResponseCode::Conflict),
        ("ContextFinished", ResponseCode::Conflict),
        ("SessionUnavailable", ResponseCode::NotFound),
        ("HostCallTimeout", ResponseCode::Unavailable),
        ("plugin_busy", ResponseCode::Conflict),
        ("unsupported", ResponseCode::BadRequest),
    ] {
        if text.contains(hint) {
            return code;
        }
    }
    ResponseCode::InternalError
}
#[async_trait::async_trait]
impl PluginExecutor for PythonExecutor {
    fn status(&self) -> Value {
        let calls = self.worker.calls.lock().unwrap();
        json!({"healthy":self.worker.healthy.load(Ordering::SeqCst),
            "pid":self.worker._child.try_lock().ok().and_then(|child|child.id()),
            "last_error":self.worker.last_error.lock().unwrap().clone(),
            "active_calls":calls.values().filter_map(Weak::upgrade)
                .filter(|c| !*c.released.borrow() && !c.streaming.load(Ordering::SeqCst)).count(),
            "active_streams":calls.values().filter_map(Weak::upgrade)
                .filter(|c| !*c.released.borrow() && c.streaming.load(Ordering::SeqCst)).count(),
            "pending_requests":self.worker.pending.lock().unwrap().len()})
    }
    async fn invoke(
        &self,
        ctx: &Ctx,
        root: &Ctx,
        point: &str,
        method: &str,
        location: Option<&str>,
        metadata: Value,
        argument: PluginValue,
    ) -> anyhow::Result<PluginValue> {
        let result=async {
        anyhow::ensure!(
            self.worker.healthy.load(Ordering::SeqCst),
            "plugin_worker_unavailable"
        );
        let id = wd_tools::uuid::v4();
        // Model matching can refresh provider credentials and discover remote models.
        // Give it the same I/O budget as model execution.
        let budget = if point == "ModelHook" || matches!(method, "on_model_resp" | "on_query") {
            120_000
        } else {
            1000
        };
        let members = self
            .members
            .iter()
            .flat_map(|r| {
                r.declaration.mounts.iter().enumerate().map(move |(i, _)| {
                    (format!("{}:{i}", r.id), (r.id.clone(), r.revision.clone()))
                })
            })
            .collect::<HashMap<_, _>>();
        let mut contexts = HashMap::new();
        let mut payload = serde_json::Map::new();
        for key in members.keys() {
            contexts.insert(key.clone(), ctx.clone());
            payload.insert(
                key.clone(),
                serde_json::to_value(plugin::codec::context(ctx, key))?,
            );
        }
        let call = Arc::new(PluginCall {
            id: id.clone(),
            worker: Arc::downgrade(&self.worker),
            ctx: ctx.clone(),
            root: root.clone(),
            metadata,
            point: point.into(),
            method: method.into(),
            next_members: Mutex::new(std::collections::HashSet::new()),
            deadline: Instant::now() + Duration::from_millis(budget),
            members,
            contexts: Mutex::new(contexts),
            streams: tokio::sync::Mutex::new(HashMap::new()),
            tool: Mutex::new(None),
            receipts: tokio::sync::Mutex::new(HashMap::new()),
            query_completion: Mutex::new(None),
            released: tokio::sync::watch::channel(false).0,
            release_requested: AtomicBool::new(false),
            terminal_controls: AtomicUsize::new(0),
            streaming: AtomicBool::new(false),
        });
        let lease = Lease(call.clone());
        let argument = match argument {
            PluginValue::Json(v) => v,
            PluginValue::Tool(tool) => {
                let value = json!({"result":tool.plugin_result(),"reference":"input-tool"});
                *call.tool.lock().unwrap() = Some(tool);
                value
            }
            PluginValue::Model(ModelResponse::Completed(response)) => {
                json!({"kind":"completed","response":response})
            }
            PluginValue::Model(ModelResponse::Streaming(stream)) => {
                call.streams
                    .lock()
                    .await
                    .insert("input-stream".into(), stream);
                json!({"kind":"streaming","stream_id":"input-stream"})
            }
        };
        {
            let mut calls = self.worker.calls.lock().unwrap();
            anyhow::ensure!(calls.len() < 96, "plugin_busy: calls");
            calls.insert(id.clone(), Arc::downgrade(&call));
        }
        let execution = ctx.scope().ok().and_then(|s| s.execution.clone());
        if let Some(binding) = &execution {
            binding.session.report(&binding.execution_id, ExecutionReport::ResourceRegistered {
                invocation_id: binding.invocation_id.clone(),
                resource_id: RequestId::new(id.clone())?,
            })?;
        }
        let watched = call.clone();
        let mut failed = self.worker.failed.subscribe();
        tokio::spawn(async move {
            let mut released = watched.released.subscribe();
            if !*released.borrow() {
                tokio::select! {
                _=watched.root.terminal()=>{},
                _=watched.ctx.terminal()=>{},
                _=watched.ctx.cancellation()=>{},
                _=tokio::time::sleep_until(watched.deadline)=>{},
                _=failed.changed()=>{},
                _=released.changed()=>{},
                }
            }
            watched.release();
            // Terminal RPC acknowledgement can defer invalidation briefly.
            if !*released.borrow() {
                let _ = released.changed().await;
            }
            if let Some(worker) = watched.worker.upgrade() {
                // Release is a correlated control request; its reply proves Python
                // closed generators and stopped the owned business task.
                if worker.rpc(RequestPath::PluginCallRelease,json!({"call_id":watched.id}),
                    Instant::now() + Duration::from_secs(5)).await.is_err() {
                    worker.fail("plugin cleanup failed");
                    let mut child = worker._child.lock().await;
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                worker.calls.lock().unwrap().remove(&watched.id);
            }
            watched.streams.lock().await.clear();
            watched.tool.lock().unwrap().take();
            let contexts = std::mem::take(&mut *watched.contexts.lock().unwrap());
            for (handle, child) in contexts {
                if !handle.contains("/child/") { continue; }
                if let Ok(scope) = child.scope() {
                    if let Some(binding) = &scope.execution {
                        let _ = binding.session.cleaned(&binding.execution_id, &binding.invocation_id);
                    }
                }
                drop(child);
            }
            if let Some(binding) = execution {
                let _ = binding.session.report(&binding.execution_id,
                    ExecutionReport::ResourceReleased(RequestId::new(watched.id.clone()).unwrap()));
            }
        });
        let group = ctx
            .scope()
            .ok()
            .and_then(|s| {
                s.session
                    .as_ref()
                    .map(|b| json!([s.user.user_id,b.session_id]).to_string())
            })
            .unwrap_or_else(|| {
                root.plugin_execution()
                    .map(|r| r.execution_id.to_string())
                    .unwrap_or_else(|| "static".into())
            });
        let result = tokio::select! {
            result=self.worker.rpc_id(id.clone(),RequestPath::PluginInvoke,json!({"point":point,"method":method,"location":location,
                "contexts":payload,"argument":argument,"group":group,"timeout_ms":budget,
                "model_members":call.metadata.get("model_members")}),call.deadline)=>result?,
            _=root.terminal()=>anyhow::bail!("ContextFinished"),
            _=ctx.terminal()=>anyhow::bail!("ContextFinished"),
            _=ctx.cancellation()=>anyhow::bail!("ContextFinished"),
        };
        anyhow::ensure!(
            !root.state().current().is_terminal() && !ctx.state().current().is_terminal(),
            "ContextFinished"
        );
        protocol::validate_hook_value(point, method, &result, true)?;
        if let Some(text) = call.query_completion.lock().unwrap().take() {
            ctx.initial_input_applied()?;
            ctx.set_state(ContextState::succeeded(Value::String(text)))?;
            return Ok(PluginValue::Json(result));
        }
        if (point == "ModelHook" && method == "exec") || method == "on_model_resp" {
            let model = match result["kind"].as_str() {
                Some("completed") => {
                    ModelResponse::Completed(serde_json::from_value(result["response"].clone())?)
                }
                Some("host_stream") => {
                    let stream_id = result["stream_id"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("invalid stream reference"))?
                        .to_owned();
                    anyhow::ensure!(
                        call.streams.lock().await.contains_key(&stream_id),
                        "invalid stream reference"
                    );
                    call.streaming.store(true, Ordering::SeqCst);
                    // Keep the invocation lease for passthrough streams too.
                    // Pull directly from Rust; no chunk RPC to Python is needed.
                    let stream = futures_util::stream::unfold(
                        (lease, stream_id, false, true),
                        |(lease, id, done, first)| async move {
                            if done {
                                return None;
                            }
                            let next = {
                                let call = &lease.0;
                                let mut streams = call.streams.lock().await;
                                match streams.get_mut(&id) {
                                    Some(stream) => {
                                        let deadline = std::cmp::min(
                                            call.deadline,
                                            Instant::now()
                                                + Duration::from_secs(if first { 30 } else { 60 }),
                                        );
                                        tokio::select! {
                                            next=tokio::time::timeout_at(deadline,stream.next())=>
                                                next.unwrap_or_else(|_|Some(Err(OpenAIError::InvalidArgument("plugin_call_timeout".into())))),
                                            _=call.root.terminal()=>Some(Err(OpenAIError::InvalidArgument("ContextFinished".into()))),
                                            _=call.ctx.terminal()=>Some(Err(OpenAIError::InvalidArgument("ContextFinished".into()))),
                                        }
                                    }
                                    None => Some(Err(OpenAIError::InvalidArgument(
                                        "HandleExpired".into(),
                                    ))),
                                }
                            };
                            next.map(|chunk| {
                                let done = chunk.is_err();
                                if done { lease.0.release(); }
                                (chunk, (lease, id, done, false))
                            })
                        },
                    );
                    ModelResponse::Streaming(Box::pin(stream))
                }
                Some("streaming") => {
                    call.streaming.store(true, Ordering::SeqCst);
                    let stream_id = result["stream_id"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("invalid stream ID"))?
                        .to_owned();
                    let worker = self.worker.clone();
                    let stream = futures_util::stream::unfold(
                        (worker, lease, stream_id, false, true),
                        |(worker, lease, id, done, first)| async move {
                            if done {
                                return None;
                            }
                            let call = &lease.0;
                            let deadline = std::cmp::min(
                                call.deadline,
                                Instant::now() + Duration::from_secs(if first { 30 } else { 60 }),
                            );
                            let response = tokio::select! {
                                r=worker.rpc(RequestPath::PluginStreamNext,json!({"stream_id":id,"call_id":call.id,"access":null}),deadline)=>r,
                                _=call.root.terminal()=>Err(anyhow::anyhow!("ContextFinished")),
                                _=call.ctx.terminal()=>Err(anyhow::anyhow!("ContextFinished")),
                            };
                            match response {
                                Ok(value) if value["eof"] == true => None,
                                Ok(value) => Some((
                                    serde_json::from_value(value["chunk"].clone()).map_err(|e| {
                                        OpenAIError::JSONDeserialize(e, value["chunk"].to_string())
                                    }),
                                    (worker, lease, id, false, false),
                                )),
                                Err(error) => Some((
                                    Err(OpenAIError::InvalidArgument(error.to_string())),
                                    {
                                        lease.0.release();
                                        (worker, lease, id, true, false)
                                    },
                                )),
                            }
                        },
                    );
                    ModelResponse::Streaming(Box::pin(stream))
                }
                _ => anyhow::bail!("invalid model response"),
            };
            Ok(PluginValue::Model(model))
        } else if method == "on_tools_resp" || (point == "ToolsHook" && method == "exec") {
            let tool = if result["reference"] == "input-tool" {
                call.tool
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("invalid tool reference"))?
            } else {
                ToolResponse::with_result(
                    result["result"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("invalid tool response"))?
                        .into(),
                )
            };
            Ok(PluginValue::Tool(tool))
        } else {
            Ok(PluginValue::Json(result))
        }
        }.await;
        if let Err(error) = &result {
            *self.worker.last_error.lock().unwrap() = Some(error.to_string());
        }
        result
    }
}

mod capabilities;
