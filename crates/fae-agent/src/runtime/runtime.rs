use crate::common::AnyType;
use crate::{
    Event, EventType, TaskError, TaskReq, TaskRequest, TaskResp, TaskResponse, TaskType, common,
};
use std::fmt::Debug;
use std::ops::Deref;
use std::sync::Arc;
use wd_tools::channel::Receiver;

#[async_trait::async_trait]
pub trait Runtime: Debug + Send + Sync + 'static {
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
    pub fn new(env: Arc<dyn Runtime>) -> Self {
        Self(env)
    }
    pub(crate) fn null() -> Self {
        Self(Arc::new(RuntimeNull))
    }

    pub async fn spawn<Req>(&self, tasks: TaskReq<Req>) -> anyhow::Result<()>
    where
        Req: Send + 'static,
    {
        let mut task = tasks.into_request();
        self.0.spawn(&mut task).await?;
        Ok(())
    }

    pub async fn exec<Req, Resp>(&self, task: TaskReq<Req>) -> anyhow::Result<TaskResp<Resp>>
    where
        Req: Send + 'static,
        Resp: Send + 'static,
    {
        let mut task = task.into_request();
        let mut response = self.0.exec(&mut task).await?;
        TaskResp::<Resp>::try_from_response(&mut response).ok_or_else(|| {
            anyhow::anyhow!(
                "task response type does not match `{}`",
                std::any::type_name::<Resp>()
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
    fn id(&self) -> &str;
    fn tys(&self) -> Vec<TaskType>;
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
}

#[async_trait::async_trait]
impl<Req, Resp, Cond, Info> Runtime for RuntimeSelectExecWrapped<Req, Resp, Cond, Info>
where
    Req: Debug + Send + 'static,
    Resp: Debug + Send + 'static,
    Cond: Debug + Send + 'static,
    Info: Debug + Send + 'static,
{
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
        if task.meta.executor != self.id() {
            return Err(crate::Error::RuntimeNoSupport);
        }
        let req = if let Some(req) = TaskReq::<Req>::try_from_request(task) {
            req
        } else {
            return Err(crate::Error::RuntimeNoSupport);
        };
        if req.ctx.is_aborted() {
            return Err(crate::Error::ContextAborted);
        }
        let info = format!("{:?}", req.meta);
        req.ctx.append_stack(self.id(), info);
        self.inner.spawn(req).await
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
        if task.meta.executor != self.id() {
            return Err(crate::Error::RuntimeNoSupport);
        }
        let req = if let Some(req) = TaskReq::<Req>::try_from_request(task) {
            req
        } else {
            return Err(crate::Error::RuntimeNoSupport);
        };
        if req.ctx.is_aborted() {
            return Err(crate::Error::ContextAborted);
        }
        let info = format!("{:?}", req.meta);
        req.ctx.append_stack(self.id(), info);
        let resp = self.inner.exec(req).await?;
        Ok(resp.into_response())
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
