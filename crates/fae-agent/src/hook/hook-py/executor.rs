//! Framed multiplexed Python bridge. The read loop never waits for business work.
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
    let value = timeout(Duration::from_secs(5), read_frame(&mut stdout)).await??;
    let status = timeout(Duration::from_secs(5), child.wait()).await??;
    anyhow::ensure!(
        status.success() && value.get("error").is_none(),
        "plugin_config_invalid: {}",
        value
    );
    Ok(
        serde_json::from_value::<PluginDeclaration>(value["result"].clone())?
            .normalize(id, Some(PluginEngine::Py))?,
    )
}

#[derive(Debug)]
pub struct PythonExecutor {
    worker: Arc<Worker>,
    members: Vec<PluginRecord>,
}
struct Worker {
    _child: Mutex<Child>,
    write: mpsc::Sender<Value>,
    control: mpsc::Sender<Value>,
    pending: Mutex<HashMap<String, Reply>>,
    calls: Mutex<HashMap<String, Weak<Call>>>,
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
struct Call {
    id: String,
    worker: Weak<Worker>,
    ctx: Ctx,
    root: Ctx,
    metadata: Value,
    deadline: Instant,
    members: HashMap<String, (String, String)>,
    contexts: Mutex<HashMap<String, Ctx>>,
    views: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<ContextStateView>>>>,
    streams: tokio::sync::Mutex<HashMap<String, ChatCompletionResponseStream>>,
    tool: Mutex<Option<ToolResponse>>,
    receipts: tokio::sync::Mutex<HashMap<String, Value>>,
    released: tokio::sync::watch::Sender<bool>,
    release_requested: AtomicBool,
    terminal_controls: AtomicUsize,
    streaming: AtomicBool,
    json_output: bool,
}
// A state-changing RPC must enqueue its acknowledgement before invalidation,
// even if an outer runtime drops the invocation as soon as the state changes.
struct TerminalControl(Arc<Call>);
impl Drop for TerminalControl {
    fn drop(&mut self) {
        if self.0.terminal_controls.fetch_sub(1, Ordering::SeqCst) == 1
            && self.0.release_requested.load(Ordering::SeqCst)
        {
            self.0.release();
        }
    }
}
struct Lease(Arc<Call>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.release();
    }
}
impl Call {
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
        if let Some(worker) = self.worker.upgrade() {
            worker.calls.lock().unwrap().remove(&self.id);
            let frame = json!({"method":"release","params":{"call_id":self.id}});
            if let Err(mpsc::error::TrySendError::Full(frame)) = worker.control.try_send(frame) {
                let sender = worker.control.clone();
                tokio::spawn(async move {
                    let _ = sender.send(frame).await;
                });
            }
        }
    }
}
impl Worker {
    async fn rpc(&self, method: &str, params: Value, deadline: Instant) -> anyhow::Result<Value> {
        let id = wd_tools::uuid::v4();
        self.rpc_id(id, method, params, deadline).await
    }
    async fn rpc_id(
        &self,
        id: String,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> anyhow::Result<Value> {
        let (send, receive) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            anyhow::ensure!(pending.len() < 256, "plugin_busy: requests");
            pending.insert(id.clone(), send);
        }
        let _pending = Pending {
            worker: self,
            id: id.clone(),
        };
        let result = tokio::time::timeout_at(deadline, async {
            self.write
                .send(json!({"id":id,"method":method,"params":params}))
                .await?;
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
        for (_, reply) in self.pending.lock().unwrap().drain() {
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
        let ready = timeout(Duration::from_secs(5), read_frame(&mut stdout)).await??;
        let _ = tokio::fs::remove_file(&file).await;
        anyhow::ensure!(
            ready["method"] == "ready"
                && ready["params"]["protocol_version"] == 1
                && ready["params"]["api_version"] == API_VERSION,
            "plugin_startup_failed: {ready}"
        );
        let (write, mut queue) = mpsc::channel::<Value>(128);
        let (control, mut controls) = mpsc::channel::<Value>(64);
        let worker = Arc::new(Worker {
            _child: Mutex::new(child),
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
                let id = frame["id"].as_str().unwrap_or("").to_owned();
                if frame["method"] == "host.call" {
                    let call_id = frame["params"]["call_id"].as_str().unwrap_or("");
                    let call = worker
                        .calls
                        .lock()
                        .unwrap()
                        .get(call_id)
                        .and_then(Weak::upgrade);
                    let permit = worker.host_slots.clone().try_acquire_owned();
                    let sender = worker.control.clone();
                    let control = call
                        .as_ref()
                        .filter(|_| {
                            matches!(
                                frame["params"]["method"].as_str(),
                                Some("context.cancel" | "context.cancel_run" | "context.set_state")
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
                                    r=tokio::time::timeout_at(call.deadline,call.host(&id,&frame["params"]))=>
                                        r.unwrap_or_else(|_|Err(anyhow::anyhow!("HostCallTimeout"))),
                                    _=released.changed()=>Err(anyhow::anyhow!("HandleExpired")),
                                }
                            }
                            (None, _) => Err(anyhow::anyhow!("HandleExpired")),
                            _ => Err(anyhow::anyhow!("plugin_busy: host_requests")),
                        };
                        let reply = match result {
                            Ok(result) => json!({"id":id,"result":result}),
                            Err(error) => {
                                let text = error.to_string();
                                json!({"id":id,"error":{"code":error_code(&error),"message":text,
                                    "resource":text.strip_prefix("plugin_busy: ")}})
                            }
                        };
                        let _ = sender.send(reply).await;
                        drop(control);
                    });
                } else if let Some(reply) = worker.pending.lock().unwrap().remove(&id) {
                    let result = if let Some(error) = frame.get("error") {
                        Err(anyhow::anyhow!("Python plugin: {error}"))
                    } else {
                        Ok(frame["result"].clone())
                    };
                    let _ = reply.send(result);
                }
            }
        });
        Ok(Self { worker, members })
    }
}
fn error_code(error: &anyhow::Error) -> &str {
    let text = error.to_string();
    for (hint, code) in [
        ("HandleExpired", "handle_expired"),
        ("terminal", "context_finished"),
        ("ContextFinished", "context_finished"),
        ("ContextUnavailable", "context_unavailable"),
        ("not bound", "context_unavailable"),
        ("StateViewClosed", "state_view_closed"),
        ("UnsupportedOutputType", "unsupported_output_type"),
        ("SessionUnavailable", "session_unavailable"),
        ("SessionBusy", "session_busy"),
        ("HostCallTimeout", "host_call_timeout"),
        ("plugin_busy", "plugin_busy"),
        ("already bound", "scope_already_bound"),
        ("InvalidStateTransition", "invalid_state_transition"),
        ("InvalidScope", "invalid_scope"),
        ("stack capacity", "stack_capacity_exceeded"),
        ("invalid context state", "invalid_state_transition"),
    ] {
        if text.contains(hint) {
            return code;
        }
    }
    "plugin_error"
}
#[async_trait::async_trait]
impl PluginExecutor for PythonExecutor {
    fn status(&self) -> Value {
        let calls = self.worker.calls.lock().unwrap();
        json!({"healthy":self.worker.healthy.load(Ordering::SeqCst),
            "pid":self.worker._child.lock().unwrap().id(),
            "last_error":self.worker.last_error.lock().unwrap().clone(),
            "active_calls":calls.values().filter_map(Weak::upgrade)
                .filter(|c| !c.streaming.load(Ordering::SeqCst)).count(),
            "active_streams":calls.values().filter_map(Weak::upgrade)
                .filter(|c| c.streaming.load(Ordering::SeqCst)).count(),
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
        let budget = if method != "match_model" && (point == "ModelHook" || method == "on_model_resp") {
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
                plugin::codec::context(ctx, key, root, &metadata),
            );
        }
        let call = Arc::new(Call {
            id: id.clone(),
            worker: Arc::downgrade(&self.worker),
            ctx: ctx.clone(),
            root: root.clone(),
            metadata,
            deadline: Instant::now() + Duration::from_millis(budget),
            members,
            contexts: Mutex::new(contexts),
            views: tokio::sync::Mutex::new(HashMap::new()),
            streams: tokio::sync::Mutex::new(HashMap::new()),
            tool: Mutex::new(None),
            receipts: tokio::sync::Mutex::new(HashMap::new()),
            released: tokio::sync::watch::channel(false).0,
            release_requested: AtomicBool::new(false),
            terminal_controls: AtomicUsize::new(0),
            streaming: AtomicBool::new(false),
            // SingleAgentPlan outputs serde_json::Value; model/tool tasks own
            // native response types. Unscheduled plugin children accept JSON.
            json_output: point == "SingleAgentHook",
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
        let watched = call.clone();
        let mut failed = self.worker.failed.subscribe();
        tokio::spawn(async move {
            let mut released = watched.released.subscribe();
            if !*released.borrow() {
                tokio::select! {
                _=watched.root.terminal()=>{},
                _=watched.ctx.terminal()=>{},
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
            watched.streams.lock().await.clear();
            watched.views.lock().await.clear();
            watched.tool.lock().unwrap().take();
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
                root.plugin_run()
                    .map(|r| r.run_id.clone())
                    .unwrap_or_else(|| "static".into())
            });
        let result = tokio::select! {
            result=self.worker.rpc_id(id.clone(),"call",json!({"point":point,"method":method,"location":location,
                "contexts":payload,"argument":argument,"group":group,"timeout_ms":budget,
                "model_members":call.metadata.get("model_members")}),call.deadline)=>result?,
            _=root.terminal()=>anyhow::bail!("ContextFinished"),
            _=ctx.terminal()=>anyhow::bail!("ContextFinished"),
        };
        anyhow::ensure!(
            !root.state().current().is_terminal() && !ctx.state().current().is_terminal(),
            "ContextFinished"
        );
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
                                r=worker.rpc("stream.next",json!({"stream_id":id}),deadline)=>r,
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

impl Call {
    async fn host(&self, request_id: &str, frame: &Value) -> anyhow::Result<Value> {
        anyhow::ensure!(!*self.released.borrow(), "HandleExpired");
        let member = frame["member"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
        let (plugin_id, revision) = self
            .members
            .get(member)
            .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
        let method = frame["method"].as_str().unwrap_or("");
        let p = &frame["params"];
        let handle = p["handle"].as_str().unwrap_or(member);
        if method == "output.json_snapshot" && handle == "unsupported-output" {
            anyhow::bail!("UnsupportedOutputType");
        }
        let ctx = self
            .contexts
            .lock()
            .unwrap()
            .get(handle)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
        anyhow::ensure!(
            handle == member || handle.starts_with(&format!("{member}/")),
            "HandleExpired"
        );
        anyhow::ensure!(
            !self.root.state().current().is_terminal() || method == "context.cancel_run",
            "ContextFinished"
        );
        if method == "context.get_engine" {
            return Ok(json!({"engine_id":format!("{:p}",Arc::as_ptr(&ctx.get_engine()))}));
        }
        if method == "context.get_session" {
            return plugin::codec::session(&ctx, handle);
        }
        if method == "context.scope" {
            return plugin::codec::scope(&ctx, handle);
        }
        if method == "output.json_snapshot" {
            anyhow::bail!("UnsupportedOutputType");
        }
        if method.starts_with("stream.") {
            let stream_id = p["stream_id"].as_str().unwrap_or("");
            let mut streams = self.streams.lock().await;
            if method == "stream.close" {
                streams.remove(stream_id);
                return Ok(Value::Null);
            }
            let stream = streams
                .get_mut(stream_id)
                .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
            return match stream.next().await {
                Some(chunk) => Ok(json!({"eof":false,"chunk":chunk?})),
                None => Ok(json!({"eof":true})),
            };
        }
        if method.starts_with("state.") {
            if method == "state.subscribe" {
                let id = format!("{member}/state/{}", wd_tools::uuid::v4());
                let mut views = self.views.lock().await;
                anyhow::ensure!(views.len() < 64, "plugin_busy: subscriptions");
                views.insert(id.clone(), Arc::new(tokio::sync::Mutex::new(ctx.state())));
                self.contexts.lock().unwrap().insert(id.clone(), ctx);
                return Ok(json!({"handle":id}));
            }
            if method == "state.close" {
                self.views.lock().await.remove(handle);
                self.contexts.lock().unwrap().remove(handle);
                return Ok(Value::Null);
            }
            let view = self
                .views
                .lock()
                .await
                .get(handle)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("StateViewClosed"))?;
            if method == "state.changed" {
                view.lock()
                    .await
                    .changed()
                    .await
                    .map_err(|_| anyhow::anyhow!("StateViewClosed"))?;
                return Ok(Value::Null);
            }
            return Ok(plugin::codec::state(&ctx.state().current()));
        }
        if method == "context.stacks" {
            let q = &p["query"];
            return plugin::codec::stacks(ctx.stacks(ContextStackQuery {
                tree: q["tree"].as_bool().unwrap_or(false),
                task_id: string(q, "task_id"),
                plan_id: string(q, "plan_id"),
                executor: string(q, "executor"),
                phase: if q["phase"].is_null() {
                    None
                } else {
                    Some(plugin::codec::phase(&q["phase"])?)
                },
                after_sequence: q["after_sequence"].as_u64(),
                limit: q["limit"].as_u64().map(|v| v as usize),
            })?);
        }
        // Mutation IDs are idempotent throughout an invocation.
        let mut receipts = self.receipts.lock().await;
        if let Some(value) = receipts.get(request_id) {
            return Ok(value.clone());
        }
        anyhow::ensure!(receipts.len() < 256, "plugin_busy: receipts");
        let result = match method {
            "session.send" => {
                let text = p["text"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid text"))?;
                anyhow::ensure!(
                    !text.trim().is_empty() && text.len() <= 64 * 1024,
                    "invalid message text"
                );
                let session = ctx
                    .get_session()?
                    .ok_or_else(|| anyhow::anyhow!("SessionUnavailable"))?;
                let scope = ctx.scope()?;
                anyhow::ensure!(!ctx.state().current().is_terminal(), "ContextFinished");
                let message_id = format!("{}:{request_id}", self.id);
                let data = SessionEventData::Custom {
                    event_type: "plugin_message".into(),
                    content: json!({"message_id":message_id,"plugin_id":plugin_id,"revision":revision,"text":text}),
                };
                let turn = self.metadata["turn_id"].as_u64().or(scope.turn_id);
                let mut emit = || {
                    if let (Some(agent), Some(turn)) = (&scope.agent_id, turn) {
                        session.emit_agent(turn, agent, "plugin", data.clone())
                    } else {
                        session.emit(SessionEvent {
                            workflow_id: scope.workflow_id.clone(),
                            parent_agent_name: None,
                            node_id: scope.node_id.clone(),
                            turn_id: turn,
                            agent_name: scope.agent_id.clone(),
                            source: "plugin".into(),
                            data: data.clone(),
                        })
                    }
                };
                self.root.with_active(&mut || {
                    if self.root.same_context(&ctx) {
                        emit()
                    } else {
                        ctx.with_active(&mut emit)
                    }
                })?;
                json!({"message_id":message_id})
            }
            "context.cancel" | "context.cancel_run" => {
                let target = if method == "context.cancel_run" {
                    &self.root
                } else {
                    &ctx
                };
                if !target.state().current().is_cancelled() {
                    target.set_state(ContextState::cancelled(
                        p["reason"].as_str().unwrap_or("Cancelled by plugin"),
                    ))?;
                }
                if self.ctx.state().current().is_terminal()
                    || self.root.state().current().is_terminal()
                {
                    json!({"_invocation_terminal":true,"result":null})
                } else {
                    Value::Null
                }
            }
            "context.set_state" => {
                let state = &p["state"];
                let state = match state["kind"].as_str() {
                    Some("created") => ContextState::Created,
                    Some("running") => ContextState::Running,
                    Some("failed") => ContextState::failed(
                        state["error"]["message"]
                            .as_str()
                            .unwrap_or("Plugin failed"),
                    ),
                    Some("cancelled") => ContextState::cancelled(
                        state["reason"].as_str().unwrap_or("Plugin cancelled"),
                    ),
                    Some("succeeded") => {
                        anyhow::ensure!(
                            handle.starts_with(&format!("{member}/child/"))
                                || (handle == member && self.json_output),
                            "UnsupportedOutputType"
                        );
                        ContextState::succeeded(state["output"]["json"].clone())
                    }
                    _ => anyhow::bail!("invalid state"),
                };
                let result = json!(match ctx.set_state(state)? {
                    ContextStateUpdate::Applied => "applied",
                    ContextStateUpdate::Unchanged => "unchanged",
                });
                if self.ctx.state().current().is_terminal()
                    || self.root.state().current().is_terminal()
                {
                    json!({"_invocation_terminal":true,"result":result})
                } else {
                    result
                }
            }
            "context.append_stack" => {
                let entry = &p["entry"];
                json!(ctx.append_stack(ContextStackDraft {
                    task_meta: plugin::codec::meta(&entry["task_meta"])?,
                    phase: plugin::codec::phase(&entry["phase"])?,
                    parent_entry_id: entry["parent_entry_id"].as_u64(),
                    elapsed_ms: entry["elapsed_ms"].as_u64(),
                    message: string(entry, "message")
                })?)
            }
            "context.fork" => {
                let spec = &p["spec"];
                let parent = ctx.scope()?;
                let session = if spec["session"].is_null() {
                    None
                } else {
                    let h = spec["session"]["session"]["handle"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
                    anyhow::ensure!(
                        h == member || h.starts_with(&format!("{member}/")),
                        "HandleExpired"
                    );
                    self.contexts
                        .lock()
                        .unwrap()
                        .get(h)
                        .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?
                        .scope()?
                        .session
                        .clone()
                };
                let todo = match spec["todo"]["kind"].as_str().unwrap_or("auto") {
                    "auto" => TodoContextBinding::Auto,
                    "inherit" => TodoContextBinding::Inherit,
                    "clear" => TodoContextBinding::Clear,
                    "bind" => {
                        anyhow::ensure!(spec["todo"]["scope"]["handle"] == handle, "HandleExpired");
                        TodoContextBinding::Bind(
                            parent
                                .todo
                                .clone()
                                .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?,
                        )
                    }
                    _ => anyhow::bail!("invalid todo binding"),
                };
                anyhow::ensure!(
                    self.contexts.lock().unwrap().len() < 128,
                    "plugin_busy: contexts"
                );
                let child = ctx.fork(ContextChildSpec {
                    timer_source: if spec["timer_source"].is_null() {
                        None
                    } else {
                        Some(plugin::codec::source_from_json(&spec["timer_source"])?)
                    },
                    timer_workspace: string(spec, "timer_workspace").map(PathBuf::from),
                    session,
                    agent_id: string(spec, "agent_id"),
                    workflow_id: string(spec, "workflow_id"),
                    node_id: string(spec, "node_id"),
                    turn_id: spec["turn_id"].as_u64(),
                    execution: spec["execution"].as_bool().unwrap_or(false),
                    workspace: string(spec, "workspace").map(PathBuf::from),
                    todo,
                    ..Default::default()
                })?;
                let id = format!("{member}/child/{}", wd_tools::uuid::v4());
                let result = plugin::codec::context(&child, &id, &self.root, &self.metadata);
                self.contexts.lock().unwrap().insert(id, child);
                result
            }
            "context.bind_scope" => {
                if ctx.scope().is_ok() {
                    anyhow::bail!("scope already bound");
                }
                let scope = &p["scope"];
                anyhow::ensure!(
                    matches!(*ctx.state().current(), ContextState::Created),
                    "InvalidStateTransition"
                );
                // Only user identity already supplied by this invocation may be used.
                let user = self
                    .ctx
                    .scope()
                    .map_err(|_| anyhow::anyhow!("ContextUnavailable"))?
                    .user
                    .clone();
                anyhow::ensure!(scope["user"]["user_id"] == user.user_id, "InvalidScope");
                anyhow::ensure!(
                    scope["session"].is_null()
                        && scope["todo"].is_null()
                        && scope["timer_execution"].is_null()
                        && scope["parent_scope_id"].is_null(),
                    "InvalidScope: unbound root cannot fabricate host references"
                );
                let mut binding = ContextScope::root(user.user_id.clone());
                binding.scope_id = scope["scope_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("InvalidScope"))?
                    .into();
                binding.user = user;
                binding.agent_id = string(scope, "agent_id");
                binding.workflow_id = string(scope, "workflow_id");
                binding.node_id = string(scope, "node_id");
                binding.turn_id = scope["turn_id"].as_u64();
                binding.workspace = string(scope, "workspace").map(PathBuf::from);
                binding.execution_origin = string(scope, "execution_origin");
                ctx.bind_scope(binding)?;
                plugin::codec::context(&ctx, handle, &self.root, &self.metadata)
            }
            _ => anyhow::bail!("unsupported host method {method}"),
        };
        receipts.insert(request_id.into(), result.clone());
        Ok(result)
    }
}
fn string(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_owned)
}
