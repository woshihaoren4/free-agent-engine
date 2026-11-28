use crate::common::AnyType;
use crate::{
    ContextStackDraft, ContextStackPhase, ContextState, Event, EventType, TaskError, TaskReq,
    TaskRequest, TaskResp, TaskResponse, TaskType, common,
};
use std::fmt::Debug;
use std::ops::Deref;
use std::sync::Arc;
use wd_tools::channel::Receiver;

#[async_trait::async_trait]
pub trait Runtime: Debug + Send + Sync + 'static {
    fn descriptors(&self) -> Vec<crate::RuntimeDescriptor> {
        Vec::new()
    }
    async fn select_on(
        &self,
        _ctx: &crate::Ctx,
        _meta: &crate::RuntimeCallMeta,
        _cond: &mut AnyType,
    ) -> crate::Result<AnyType> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn kill_on(
        &self,
        _ctx: &crate::Ctx,
        meta: &crate::RuntimeCallMeta,
        task_id: &str,
    ) -> crate::Result<()> {
        self.kill(meta.ty.clone(), &meta.runtime_id, task_id).await
    }
    async fn select_with_context(
        &self,
        _ctx: &crate::Ctx,
        ty: TaskType,
        cond: &mut common::AnyType,
    ) -> crate::Result<common::AnyType> {
        self.select(ty, cond).await
    }
    fn id(&self) -> &str;
    async fn watch(&self) -> crate::Result<wd_tools::channel::Receiver<Event>> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn select(
        &self,
        _ty: TaskType,
        _cond: &mut common::AnyType,
    ) -> crate::Result<common::AnyType> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn spawn(&self, _tasks: &mut TaskRequest) -> crate::Result<()> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn trigger(&self, _event: &mut Event) -> crate::Result<()> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn exec(&self, _task: &mut TaskRequest) -> crate::Result<TaskResponse> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn kill(&self, _ty: TaskType, _rtid: &str, _task_id: &str) -> crate::Result<()> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn exit(&self) -> crate::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct RuntimeNull;

#[async_trait::async_trait]
impl Runtime for RuntimeNull {
    fn id(&self) -> &str {
        "null"
    }
}

#[derive(Debug, Clone)]
pub struct RT(Arc<dyn Runtime>);
impl RT {
    pub async fn select_with_context<Cond: Send + 'static, Info: Send + 'static>(
        &self,
        ctx: &crate::Ctx,
        ty: TaskType,
        cond: Cond,
    ) -> anyhow::Result<Info> {
        let mut cond: AnyType = Box::new(cond);
        let info = self.0.select_with_context(ctx, ty, &mut cond).await?;
        info.downcast::<Info>()
            .map(|info| *info)
            .map_err(|_| anyhow::anyhow!("runtime select type mismatch"))
    }
    pub fn new(env: Arc<dyn Runtime>) -> Self {
        Self(env)
    }
    pub(crate) fn null() -> Self {
        Self(Arc::new(RuntimeNull))
    }

    pub async fn spawn<Req>(&self, mut tasks: TaskReq<Req>) -> anyhow::Result<()>
    where
        Req: Send + 'static,
    {
        if tasks.meta.id.is_empty() {
            tasks.meta.id = wd_tools::uuid::v4();
        }
        if tasks.meta.ty != TaskType::Plan && tasks.ctx.scope().is_ok() {
            tasks.ctx = tasks.ctx.fork(crate::ContextChildSpec::default())?;
        }
        let mut task = tasks.into_request();
        self.0.spawn(&mut task).await?;
        Ok(())
    }

    pub async fn exec<Req, Resp>(&self, mut task: TaskReq<Req>) -> anyhow::Result<TaskResp<Resp>>
    where
        Req: Send + 'static,
        Resp: Send + 'static,
    {
        if task.meta.id.is_empty() {
            task.meta.id = wd_tools::uuid::v4();
        }
        if task.meta.ty != TaskType::Plan && task.ctx.scope().is_ok() {
            task.ctx = task.ctx.fork(crate::ContextChildSpec::default())?;
        }
        let mut task = task.into_request();
        let mut response = self.0.exec(&mut task).await?;
        TaskResp::<Resp>::try_from_response(&mut response).ok_or_else(|| {
            anyhow::anyhow!(
                "task response type does not match `{}`",
                std::any::type_name::<Resp>()
            )
        })
    }

    pub async fn select<Cond, Info>(&self, ty: TaskType, cond: Cond) -> anyhow::Result<Info>
    where
        Cond: Send + 'static,
        Info: Send + 'static,
    {
        let mut cond: AnyType = Box::new(cond);
        let info = self.0.select(ty, &mut cond).await?;
        info.downcast::<Info>().map(|info| *info).map_err(|_| {
            anyhow::anyhow!(
                "runtime select response type does not match `{}`",
                std::any::type_name::<Info>()
            )
        })
    }
}
impl Deref for RT {
    type Target = dyn Runtime;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

// ---- 扩展 ----
#[async_trait::async_trait]
pub trait RuntimeSelectExec<Req, Resp, Cond, Info>: Debug + Send + Sync + 'static
where
    Req: Debug + Send + 'static,
    Resp: Debug + Send + 'static,
    Cond: Debug + Send + 'static,
    Info: Debug + Send + 'static,
{
    async fn kill_with_context(&self, _ctx: &crate::Ctx, task_id: &str) -> crate::Result<()> {
        self.kill(task_id).await
    }
    async fn select_with_context(
        &self,
        _ctx: &crate::Ctx,
        ty: TaskType,
        cond: Cond,
    ) -> crate::Result<Info> {
        self.select(ty, cond).await
    }
    fn id(&self) -> &str;
    fn tys(&self) -> Vec<TaskType>;
    /// Inspect a typed request without accepting it or starting any work.
    fn supports_task(&self, _ctx: &crate::Ctx, _meta: &crate::TaskMeta, _req: &Req) -> bool {
        true
    }
    async fn watch(&self) -> crate::Result<wd_tools::channel::Receiver<Event>> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn select(&self, _ty: TaskType, _cond: Cond) -> crate::Result<Info> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn spawn(&self, _tasks: TaskReq<Req>) -> crate::Result<()> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn trigger(&self, _event: &mut common::AnyType) -> crate::Result<()> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn task_result_callback(&self, _task: TaskResponse) -> crate::Result<()> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn task_error_callback(&self, _task: TaskError) -> crate::Result<()> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn exec(&self, _task: TaskReq<Req>) -> crate::Result<TaskResp<Resp>> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn kill(&self, _task_id: &str) -> crate::Result<()> {
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn exit(&self) -> crate::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeSelectExecWrapped<Req, Resp, Cond, Info> {
    inner: Arc<dyn RuntimeSelectExec<Req, Resp, Cond, Info>>,
    tys: Vec<TaskType>,
}
impl<Req, Resp, Cond, Info> RuntimeSelectExecWrapped<Req, Resp, Cond, Info>
where
    Req: Debug + Send + 'static,
    Resp: Debug + Send + 'static,
    Cond: Debug + Send + 'static,
    Info: Debug + Send + 'static,
{
    pub fn new(inner: Arc<dyn RuntimeSelectExec<Req, Resp, Cond, Info>>) -> Self {
        let tys = inner.tys();
        Self { inner, tys }
    }
    fn contain_ty(&self, ty: &TaskType) -> bool {
        self.tys.contains(ty)
    }

    fn take_task(&self, task: &mut TaskRequest) -> crate::Result<TaskReq<Req>> {
        if task.meta.executor != self.inner.id() || !self.contain_ty(&task.meta.ty) {
            return Err(crate::Error::RuntimeNoSupport);
        }
        let req = task
            .payload()
            .downcast_ref::<Req>()
            .ok_or(crate::Error::RuntimeNoSupport)?;
        if !self.inner.supports_task(&task.ctx, &task.meta, req) {
            return Err(crate::Error::RuntimeNoSupport);
        }
        if task.ctx.cancel_requested() {
            return Err(crate::Error::ContextAborted);
        }
        Ok(TaskReq::try_from_request(task).expect("request type checked before acceptance"))
    }

    fn accepted_error(&self, error: crate::Error) -> crate::Error {
        match error {
            crate::Error::RuntimeNoSupport => crate::Error::AcceptedTaskFailed {
                runtime_id: self.inner.id().into(),
            },
            error => error,
        }
    }
}

#[async_trait::async_trait]
impl<Req, Resp, Cond, Info> Runtime for RuntimeSelectExecWrapped<Req, Resp, Cond, Info>
where
    Req: Debug + Send + 'static,
    Resp: Debug + Send + 'static,
    Cond: Debug + Send + 'static,
    Info: Debug + Send + 'static,
{
    async fn kill_on(
        &self,
        ctx: &crate::Ctx,
        meta: &crate::RuntimeCallMeta,
        task_id: &str,
    ) -> crate::Result<()> {
        if meta.runtime_id != self.id() || !self.contain_ty(&meta.ty) {
            return Err(crate::Error::RuntimeNoSupport);
        }
        self.inner.kill_with_context(ctx, task_id).await
    }
    fn descriptors(&self) -> Vec<crate::RuntimeDescriptor> {
        vec![crate::RuntimeDescriptor {
            id: self.id().into(),
            task_types: self.tys.clone(),
        }]
    }
    async fn select_on(
        &self,
        ctx: &crate::Ctx,
        meta: &crate::RuntimeCallMeta,
        cond: &mut AnyType,
    ) -> crate::Result<AnyType> {
        if meta.runtime_id != self.id() || !self.contain_ty(&meta.ty) {
            return Err(crate::Error::RuntimeNoSupport);
        }
        self.select_with_context(ctx, meta.ty.clone(), cond).await
    }
    async fn select_with_context(
        &self,
        ctx: &crate::Ctx,
        ty: TaskType,
        cond: &mut AnyType,
    ) -> crate::Result<AnyType> {
        if cond.downcast_ref::<Cond>().is_none() {
            return Err(crate::Error::RuntimeNoSupport);
        }
        let value = std::mem::replace(cond, Box::new(()))
            .downcast::<Cond>()
            .unwrap();
        Ok(Box::new(
            self.inner.select_with_context(ctx, ty, *value).await?,
        ))
    }
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn watch(&self) -> crate::Result<Receiver<Event>> {
        self.inner.watch().await
    }

    async fn select(&self, _ty: TaskType, cond: &mut AnyType) -> crate::Result<AnyType> {
        if cond.downcast_ref::<Cond>().is_none() {
            return Err(crate::Error::RuntimeNoSupport);
        }
        let at = std::mem::replace(cond, Box::new(()));
        let cond = at.downcast::<Cond>().unwrap();
        let info = self.inner.select(_ty, *cond).await?;
        Ok(Box::new(info))
    }

    async fn spawn(&self, task: &mut TaskRequest) -> crate::Result<()> {
        let req = self.take_task(task)?;
        let ctx = req.ctx.clone();
        let meta = req.meta.clone();
        if meta.ty != TaskType::Plan {
            ctx.invocation_started(&meta)?;
        }
        if meta.ty != TaskType::Plan {
            let _ = ctx.set_state(ContextState::Running);
        }
        let _ = ctx.append_stack(ContextStackDraft::new(
            meta.clone(),
            ContextStackPhase::Dispatched,
        ));
        let result = self
            .inner
            .spawn(req)
            .await
            .map_err(|error| self.accepted_error(error));
        if let Err(error) = &result {
            let _ = ctx.set_state(ContextState::failed(error.to_string()));
            let _ = ctx.append_stack(ContextStackDraft::new(meta, ContextStackPhase::Failed));
            let _ = ctx.invocation_finished();
        }
        result
    }

    async fn trigger(&self, _event: &mut Event) -> crate::Result<()> {
        let Event { event_type, .. } = _event;
        match event_type {
            EventType::Task(req) => self.spawn(req).await,
            EventType::TaskResult(result) => {
                if result.meta.publisher != self.id() {
                    return Err(crate::Error::RuntimeNoSupport);
                }
                let response = std::mem::replace(result, TaskResponse::new_null());
                self.inner.task_result_callback(response).await
            }
            EventType::TaskError(error) => {
                if error.meta.publisher != self.id() {
                    return Err(crate::Error::RuntimeNoSupport);
                }
                let error = std::mem::replace(
                    error,
                    TaskError {
                        ctx: crate::Ctx::null(),
                        meta: crate::TaskMeta::default(),
                        error: String::new(),
                    },
                );
                self.inner.task_error_callback(error).await
            }
            EventType::Any(_id, ty) => self.inner.trigger(ty).await,
        }
    }

    async fn exec(&self, task: &mut TaskRequest) -> crate::Result<TaskResponse> {
        let req = self.take_task(task)?;
        let ctx = req.ctx.clone();
        let meta = req.meta.clone();
        if meta.ty != TaskType::Plan {
            ctx.invocation_started(&meta)?;
        }
        if meta.ty != TaskType::Plan {
            let _ = ctx.set_state(ContextState::Running);
        }
        if meta.ty != TaskType::Plan {
            let _ = ctx.append_stack(ContextStackDraft::new(
                meta.clone(),
                ContextStackPhase::Started,
            ));
        }
        let result = self
            .inner
            .exec(req)
            .await
            .map_err(|error| self.accepted_error(error));
        match result {
            Ok(resp) => {
                let mut resp = resp.into_response();
                resp.track_completion();
                // Streams finish in their consumer, after their final item.
                if meta.ty != TaskType::Model
                    && meta.ty != TaskType::Tool
                    && !crate::plugin::leases::has_resources(&ctx)
                {
                    if ctx.set_state(ContextState::succeeded(())).is_ok() {
                        let _ = ctx.append_stack(ContextStackDraft::new(
                            meta,
                            ContextStackPhase::Succeeded,
                        ));
                    }
                    let _ = ctx.invocation_finished();
                }
                Ok(resp)
            }
            Err(error) => {
                let _ = ctx.set_state(ContextState::failed(error.to_string()));
                let _ = ctx.append_stack(ContextStackDraft::new(meta, ContextStackPhase::Failed));
                let _ = ctx.invocation_finished();
                Err(error)
            }
        }
    }

    async fn kill(&self, ty: TaskType, rtid: &str, task_id: &str) -> crate::Result<()> {
        if !self.contain_ty(&ty) {
            return Err(crate::Error::RuntimeNoSupport);
        }
        if rtid != self.id() {
            return Err(crate::Error::RuntimeNoSupport);
        }
        self.inner.kill(task_id).await
    }

    async fn exit(&self) -> crate::Result<()> {
        self.inner.exit().await
    }
}
