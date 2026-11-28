//! Explicit JSON boundaries. Native execution retains its original Rust types.
use crate::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{any::TypeId, collections::HashMap, fmt::Debug, sync::Arc};

type Decoder = dyn Fn(&Ctx, Value) -> anyhow::Result<AnyType> + Send + Sync;
type Encoder = dyn Fn(&Ctx, AnyType) -> anyhow::Result<Value> + Send + Sync;

#[derive(Clone)]
pub struct ValueCodec {
    pub type_id: TypeId,
    decode: Arc<Decoder>,
    encode: Arc<Encoder>,
}
impl Debug for ValueCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ValueCodec").field(&self.type_id).finish()
    }
}
impl ValueCodec {
    pub fn new<T: Send + 'static>(
        decode: impl Fn(&Ctx, Value) -> anyhow::Result<T> + Send + Sync + 'static,
        encode: impl Fn(&Ctx, T) -> anyhow::Result<Value> + Send + Sync + 'static,
    ) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            decode: Arc::new(move |ctx, value| Ok(Box::new(decode(ctx, value)?))),
            encode: Arc::new(move |ctx, value| {
                let value = value
                    .downcast::<T>()
                    .map_err(|_| anyhow::anyhow!("bridge payload type mismatch"))?;
                encode(ctx, *value)
            }),
        }
    }
    pub fn serde<T: Serialize + DeserializeOwned + Send + 'static>() -> Self {
        Self::new::<T>(
            |_, value| Ok(serde_json::from_value(value)?),
            |_, value| Ok(serde_json::to_value(value)?),
        )
    }
    pub fn decode(&self, ctx: &Ctx, value: Value) -> anyhow::Result<AnyType> {
        (self.decode)(ctx, value)
    }
    pub fn encode(&self, ctx: &Ctx, value: AnyType) -> anyhow::Result<Value> {
        anyhow::ensure!(
            value.as_ref().type_id() == self.type_id,
            "bridge payload type mismatch"
        );
        (self.encode)(ctx, value)
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum RuntimeOperation {
    Exec,
    Select,
}

#[derive(Debug, Clone)]
pub struct RuntimeContract {
    pub request: ValueCodec,
    pub response: ValueCodec,
}

/// Published alongside an Engine, with one deterministic type per public operation.
#[derive(Debug, Clone, Default)]
pub struct BridgeRegistry {
    plans: HashMap<String, ValueCodec>,
    runtimes: HashMap<(String, TaskType, RuntimeOperation), RuntimeContract>,
    public_calls: HashMap<(String, RuntimeOperation), TaskType>,
}
impl BridgeRegistry {
    pub fn register_plan(&mut self, id: String, codec: ValueCodec) -> anyhow::Result<()> {
        anyhow::ensure!(!self.plans.contains_key(&id), "duplicate Plan bridge: {id}");
        self.plans.insert(id, codec);
        Ok(())
    }
    pub fn plan(&self, id: &str) -> Option<&ValueCodec> {
        self.plans.get(id)
    }
    pub fn register_runtime(
        &mut self,
        id: String,
        ty: TaskType,
        operation: RuntimeOperation,
        contract: RuntimeContract,
    ) -> anyhow::Result<()> {
        let key = (id, ty, operation);
        anyhow::ensure!(
            !self.runtimes.contains_key(&key),
            "duplicate Runtime bridge"
        );
        self.runtimes.insert(key, contract);
        Ok(())
    }
    pub fn runtime(
        &self,
        id: &str,
        ty: &TaskType,
        operation: RuntimeOperation,
    ) -> Option<&RuntimeContract> {
        self.runtimes.get(&(id.into(), ty.clone(), operation))
    }
    pub fn publish_runtime(
        &mut self,
        id: String,
        ty: TaskType,
        operation: RuntimeOperation,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.runtime(&id, &ty, operation).is_some(),
            "Runtime bridge is missing"
        );
        let key = (id, operation);
        anyhow::ensure!(
            !self.public_calls.contains_key(&key),
            "ambiguous public Runtime operation"
        );
        self.public_calls.insert(key, ty);
        Ok(())
    }
    pub fn public_runtime(
        &self,
        id: &str,
        operation: RuntimeOperation,
    ) -> anyhow::Result<TaskType> {
        self.public_calls
            .get(&(id.into(), operation))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Runtime operation is not public: {id}"))
    }
}

/// Converts only JSON input; typed callers keep their existing request and response.
#[derive(Debug)]
pub struct BridgedRuntime {
    pub runtime: Arc<dyn Runtime>,
    pub contracts: HashMap<(TaskType, RuntimeOperation), RuntimeContract>,
    pending: Arc<std::sync::Mutex<HashMap<(String, String), (ValueCodec, bool)>>>,
    json_runtime: bool,
}
impl BridgedRuntime {
    pub fn new(
        runtime: Arc<dyn Runtime>,
        contracts: HashMap<(TaskType, RuntimeOperation), RuntimeContract>,
    ) -> Self {
        Self {
            runtime,
            contracts,
            pending: Arc::default(),
            json_runtime: false,
        }
    }
    pub fn json_runtime(mut self) -> Self {
        self.json_runtime = true;
        self
    }
    fn prepare(
        &self,
        task: &mut TaskRequest,
    ) -> anyhow::Result<Option<(AnyType, RuntimeContract)>> {
        let Some(contract) = self
            .contracts
            .get(&(task.meta.ty.clone(), RuntimeOperation::Exec))
        else {
            return Ok(None);
        };
        if self.json_runtime {
            if task.payload().is::<Value>()
                || task.payload().as_ref().type_id() != contract.request.type_id
            {
                return Ok(None);
            }
            let original = task.take_payload();
            let value = contract.request.encode(&task.ctx, original)?;
            task.replace_payload(Box::new(value));
            // Once a typed request has been encoded, the JSON provider owns it.
            return Ok(Some((Box::new(()), contract.clone())));
        }
        let Some(value) = task.payload().downcast_ref::<Value>() else {
            return Ok(None);
        };
        let original = value.clone();
        let converted = contract.request.decode(&task.ctx, original.clone())?;
        task.replace_payload(converted);
        Ok(Some((Box::new(original), contract.clone())))
    }
}
#[async_trait::async_trait]
impl Runtime for BridgedRuntime {
    fn id(&self) -> &str {
        self.runtime.id()
    }
    fn descriptors(&self) -> Vec<RuntimeDescriptor> {
        self.runtime.descriptors()
    }
    async fn watch(&self) -> crate::Result<wd_tools::channel::Receiver<Event>> {
        let source = self.runtime.watch().await?;
        let (send, receive) = wd_tools::channel::Channel::new(256);
        let pending = self.pending.clone();
        tokio::spawn(async move {
            while let Ok(mut event) = source.recv().await {
                match &mut event.event_type {
                    EventType::TaskResult(response) => {
                        let key = (
                            response
                                .ctx
                                .scope()
                                .map(|s| s.scope_id.clone())
                                .unwrap_or_default(),
                            response.meta.id.clone(),
                        );
                        let codec = pending.lock().unwrap().remove(&key);
                        if let Some((codec, reverse)) = codec {
                            if !reverse {
                                response.track_completion();
                            }
                            let ctx = response.ctx.clone();
                            let payload = response.take_payload();
                            let converted = if reverse {
                                payload
                                    .downcast::<Value>()
                                    .map_err(|_| {
                                        anyhow::anyhow!("JSON provider result type mismatch")
                                    })
                                    .and_then(|value| codec.decode(&ctx, *value))
                            } else {
                                codec.encode(&ctx, payload).map(|v| Box::new(v) as AnyType)
                            };
                            match converted {
                                Ok(value) => {
                                    response.replace_payload(value);
                                    if reverse {
                                        response.mark_completion_tracked();
                                    }
                                }
                                Err(error) => {
                                    event.event_type = EventType::TaskError(TaskError {
                                        ctx: response.ctx.clone(),
                                        meta: response.meta.clone(),
                                        error: error.to_string(),
                                    })
                                }
                            }
                        }
                    }
                    EventType::TaskError(error) => {
                        let key = (
                            error
                                .ctx
                                .scope()
                                .map(|s| s.scope_id.clone())
                                .unwrap_or_default(),
                            error.meta.id.clone(),
                        );
                        pending.lock().unwrap().remove(&key);
                    }
                    _ => {}
                }
                if send.send(event).await.is_err() {
                    break;
                }
            }
        });
        Ok(receive)
    }
    async fn exec(&self, task: &mut TaskRequest) -> crate::Result<TaskResponse> {
        let converted = self.prepare(task)?;
        let ctx = task.ctx.clone();
        let result = self.runtime.exec(task).await;
        match (result, converted) {
            (Ok(mut response), Some((_, contract))) => {
                let ctx = response.ctx.clone();
                if self.json_runtime {
                    let value = response
                        .take_payload()
                        .downcast::<Value>()
                        .map_err(|_| anyhow::anyhow!("JSON provider result type mismatch"))?;
                    response.replace_payload(contract.response.decode(&ctx, *value)?);
                    response.mark_completion_tracked();
                } else {
                    response.track_completion();
                    let value = contract.response.encode(&ctx, response.take_payload())?;
                    response.replace_payload(Box::new(value));
                }
                Ok(response)
            }
            (Err(crate::Error::RuntimeNoSupport), Some((original, _))) => {
                if self.json_runtime {
                    return Err(anyhow::anyhow!(
                        "accepted_task_failed: provider returned NoSupport"
                    )
                    .into());
                }
                task.replace_payload(original);
                task.ctx = ctx;
                Err(crate::Error::RuntimeNoSupport)
            }
            (result, _) => result,
        }
    }
    async fn spawn(&self, task: &mut TaskRequest) -> crate::Result<()> {
        let converted = self.prepare(task)?;
        let key = (
            task.ctx
                .scope()
                .map(|s| s.scope_id.clone())
                .unwrap_or_default(),
            task.meta.id.clone(),
        );
        if let Some((_, contract)) = &converted {
            let mut pending = self.pending.lock().unwrap();
            if pending.contains_key(&key) {
                return Err(anyhow::anyhow!("duplicate active bridge task").into());
            }
            pending.insert(key.clone(), (contract.response.clone(), self.json_runtime));
        }
        let result = self.runtime.spawn(task).await;
        if result.is_err() {
            self.pending.lock().unwrap().remove(&key);
            if matches!(result, Err(crate::Error::RuntimeNoSupport)) {
                if let Some((original, _)) = converted {
                    if self.json_runtime {
                        return Err(anyhow::anyhow!(
                            "accepted_task_failed: provider returned NoSupport"
                        )
                        .into());
                    }
                    task.replace_payload(original);
                }
            }
        }
        result
    }
    async fn select_with_context(
        &self,
        ctx: &Ctx,
        ty: TaskType,
        cond: &mut AnyType,
    ) -> crate::Result<AnyType> {
        let contract = self.contracts.get(&(ty.clone(), RuntimeOperation::Select));
        if self.json_runtime {
            if cond.is::<Value>() {
                return self.runtime.select_with_context(ctx, ty, cond).await;
            }
            if let Some(contract) = contract {
                if cond.as_ref().type_id() == contract.request.type_id && !cond.is::<Value>() {
                    // Built-in provider select contracts use unit or text. Keep
                    // the caller's condition until a candidate actually accepts it.
                    let original: AnyType = if let Some(text) = cond.downcast_ref::<String>() {
                        Box::new(text.clone())
                    } else if cond.is::<()>() {
                        Box::new(())
                    } else {
                        return Err(crate::Error::RuntimeNoSupport);
                    };
                    let mut encoded: AnyType = Box::new(contract.request.encode(ctx, original)?);
                    let value = self
                        .runtime
                        .select_with_context(ctx, ty, &mut encoded)
                        .await?
                        .downcast::<Value>()
                        .map_err(|_| anyhow::anyhow!("JSON provider result type mismatch"))?;
                    return Ok(contract.response.decode(ctx, *value)?);
                }
            }
        }
        if let (Some(value), Some(contract)) = (cond.downcast_ref::<Value>(), contract) {
            let mut decoded = contract.request.decode(ctx, value.clone())?;
            let result = self
                .runtime
                .select_with_context(ctx, ty, &mut decoded)
                .await?;
            return Ok(Box::new(contract.response.encode(ctx, result)?));
        }
        self.runtime.select_with_context(ctx, ty, cond).await
    }
    async fn select(&self, ty: TaskType, cond: &mut AnyType) -> crate::Result<AnyType> {
        self.runtime.select(ty, cond).await
    }
    async fn trigger(&self, event: &mut Event) -> crate::Result<()> {
        self.runtime.trigger(event).await
    }
    async fn kill_on(&self, ctx: &Ctx, meta: &RuntimeCallMeta, id: &str) -> crate::Result<()> {
        let result = self.runtime.kill_on(ctx, meta, id).await;
        if let Ok(scope) = ctx.scope() {
            self.pending
                .lock()
                .unwrap()
                .remove(&(scope.scope_id.clone(), id.into()));
        }
        result
    }
    async fn kill(&self, ty: TaskType, rt: &str, id: &str) -> crate::Result<()> {
        self.runtime.kill(ty, rt, id).await
    }
    async fn exit(&self) -> crate::Result<()> {
        self.pending.lock().unwrap().clear();
        self.runtime.exit().await
    }
}

#[derive(Debug)]
pub struct BridgedPlanBuilder {
    pub builder: Box<dyn PlanBuilder>,
    pub codec: ValueCodec,
}
#[async_trait::async_trait]
impl PlanBuilder for BridgedPlanBuilder {
    fn ty(&self) -> String {
        self.builder.ty()
    }
    async fn prepare_child_spec(
        &self,
        ctx: &Ctx,
        env: &mut AnyType,
    ) -> anyhow::Result<ContextChildSpec> {
        if let Some(value) = env.downcast_ref::<Value>() {
            *env = self.codec.decode(ctx, value.clone())?;
        }
        self.builder.prepare_child_spec(ctx, env).await
    }
    async fn build(&self, rt: RT, ctx: Ctx, env: AnyType) -> anyhow::Result<Box<dyn Plan>> {
        let env = if env.is::<Value>() && self.codec.type_id != TypeId::of::<Value>() {
            self.codec.decode(&ctx, *env.downcast::<Value>().unwrap())?
        } else {
            env
        };
        self.builder.build(rt, ctx, env).await
    }
}
