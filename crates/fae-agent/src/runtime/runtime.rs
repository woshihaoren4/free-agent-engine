use std::fmt::{Debug, Formatter};
use std::ops::Deref;
use std::sync::Arc;
use wd_tools::channel::Receiver;
use crate::{common, Ctx, Event, TaskReq, TaskRequest, TaskResp, TaskResponse, TaskType};
use crate::common::AnyType;

#[async_trait::async_trait]
pub trait Runtime: Debug + Send + Sync + 'static {
    fn id(&self) -> &str;
    async fn watch(&self) -> crate::Result<wd_tools::channel::Receiver<Event>>{
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn select(&self,_ty:TaskType, _cond:&mut common::AnyType) -> crate::Result<common::AnyType>{
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn spawn(&self, _ctx: Ctx, _tasks:&mut TaskRequest) -> crate::Result<()>{
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn exec(&self, _ctx: Ctx, _task:&mut TaskRequest) -> crate::Result<TaskResponse>{
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn kill(&self, _task_id: &str) -> crate::Result<()>{
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn exit(&self) -> crate::Result<()>{
        Ok(())
    }
}

#[derive(Debug)]
pub struct RT(Arc<dyn Runtime>);
impl RT {
    pub fn new(env: Arc<dyn Runtime>) -> Self {
        Self(env)
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
    Req: Debug + Send  + 'static,
    Resp: Debug + Send  + 'static,
    Cond: Debug + Send  + 'static,
    Info: Debug + Send  + 'static,
{
    fn id(&self) -> &str;
    fn ty(&self)-> TaskType;
    async fn watch(&self) -> crate::Result<wd_tools::channel::Receiver<Event>>{
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn select(&self,_ty:TaskType, _cond:Cond) -> crate::Result<Info>{
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn spawn(&self, _ctx: Ctx, _tasks:TaskReq<Req>) -> crate::Result<()>{
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn exec(&self, _ctx: Ctx, _task:TaskReq<Req>) -> crate::Result<TaskResp<Resp>>{
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn kill(&self, _task_id: &str) -> crate::Result<()>{
        Err(crate::Error::RuntimeNoSupport)
    }
    async fn exit(&self) -> crate::Result<()>{
        Ok(())
    }
}

#[derive(Debug)]
pub struct RuntimeSelectExecWrapped<Req, Resp, Cond, Info> {
    inner: Box<dyn RuntimeSelectExec<Req, Resp, Cond, Info>>,
}


#[async_trait::async_trait]
impl<Req, Resp, Cond, Info> Runtime for RuntimeSelectExecWrapped<Req, Resp, Cond, Info>
where Req: Debug + Send  + 'static,
      Resp: Debug + Send  + 'static,
      Cond: Debug + Send  + 'static,
      Info: Debug + Send  + 'static,
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

    async fn spawn(&self, _ctx: Ctx, _tasks: &mut TaskRequest) -> crate::Result<()> {
        let req = if let Some(req) = TaskReq::<Req>::try_from_request(_tasks) {
            req
        }else{
            return Err(crate::Error::RuntimeNoSupport)
        };
        self.inner.spawn(_ctx, req).await
    }

    async fn exec(&self, _ctx: Ctx, _task: &mut TaskRequest) -> crate::Result<TaskResponse> {
        let req = if let Some(req) = TaskReq::<Req>::try_from_request(_task) {
            req
        }else{
            return Err(crate::Error::RuntimeNoSupport)
        };
        let resp = self.inner.exec(_ctx, req).await?;
        Ok(resp.into_response())
    }


    async fn kill(&self, _task_id: &str) -> crate::Result<()> {
        self.inner.kill(_task_id).await
    }

    async fn exit(&self) -> crate::Result<()> {
        self.inner.exit().await
    }
}