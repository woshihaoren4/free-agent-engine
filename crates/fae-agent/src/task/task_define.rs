use crate::{Ctx, common};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use wd_tools::PFSome;

#[derive(Debug, Default, Hash, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskType {
    #[default]
    Tool,
    Plan,
    Any(String),
}
//
// pub trait IntoTaskRequest{
//     fn into_task_request(self) -> common::AnyType;
// }
// impl<T> IntoTaskRequest for T where T:Any+Send+Sync+'static{
//     fn into_task_request(self) -> common::AnyType {
//         Box::new(self)
//     }
// }

#[derive(Debug, Default)]
pub struct TaskMeta {
    pub id: String,
    pub ty: TaskType,
    pub publisher: String,
    pub executor: String,
}

#[derive(Debug)]
pub struct TaskRequest {
    pub ctx: Ctx,
    pub meta: TaskMeta,
    req: common::AnyType,
}
impl TaskRequest {
    pub(crate) fn new_null() -> Self {
        Self {
            ctx: Ctx::null(),
            meta: TaskMeta::default(),
            req: Box::new(()),
        }
    }
}

#[derive(Debug)]
pub struct TaskResponse {
    pub ctx: Ctx,
    pub meta: TaskMeta,
    resp: common::AnyType,
}
impl TaskResponse {
    pub(crate) fn new_null() -> Self {
        Self {
            ctx: Ctx::null(),
            meta: TaskMeta::default(),
            resp: Box::new(()),
        }
    }
}

// --------------------------- 任务封装 ---------------------------

#[derive(Debug)]
pub struct TaskReq<T> {
    pub ctx: Ctx,
    pub meta: TaskMeta,
    pub req: T,
}
impl<T: 'static> TaskReq<T> {
    pub fn try_from_request(req: &mut TaskRequest) -> Option<Self> {
        if req.req.downcast_ref::<T>().is_none() {
            return None;
        }
        let tr = std::mem::replace(req, TaskRequest::new_null());

        Self {
            ctx: tr.ctx,
            meta: tr.meta,
            req: *(tr.req.downcast::<T>().unwrap()),
        }
        .some()
    }
}

impl<T: Send + 'static> TaskReq<T> {
    pub fn into_request(self) -> TaskRequest {
        TaskRequest {
            ctx: self.ctx,
            meta: self.meta,
            req: Box::new(self.req),
        }
    }
}
#[derive(Debug)]
pub struct TaskResp<T> {
    pub ctx: Ctx,
    pub meta: TaskMeta,
    pub resp: T,
}
impl<T: Send + 'static> TaskResp<T> {
    pub fn into_response(self) -> TaskResponse {
        TaskResponse {
            ctx: self.ctx,
            meta: self.meta,
            resp: Box::new(self.resp),
        }
    }
    pub fn try_from_response(resp: &mut TaskResponse) -> Option<Self> {
        if resp.resp.downcast_ref::<T>().is_none() {
            return None;
        }
        let tr = std::mem::replace(resp, TaskResponse::new_null());

        Self {
            ctx: tr.ctx,
            meta: tr.meta,
            resp: *(tr.resp.downcast::<T>().unwrap()),
        }
        .some()
    }
}
