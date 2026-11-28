use crate::{Ctx, common};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use wd_tools::PFSome;

#[derive(Debug, Default, Hash, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskType {
    #[default]
    Tool,
    Skill,
    Mcp,
    Plan,
    Workflow,
    Model,
    Session,
    Memory,
    Any(String),
}

impl TaskType {
    /// Declaration names are case sensitive; built-in names cannot denote Any.
    pub fn from_name(name: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !name.is_empty() && name.trim() == name && !name.chars().any(char::is_control),
            "invalid TaskType name"
        );
        Ok(match name {
            "Tool" => Self::Tool,
            "Skill" => Self::Skill,
            "Mcp" => Self::Mcp,
            "Plan" => Self::Plan,
            "Workflow" => Self::Workflow,
            "Model" => Self::Model,
            "Session" => Self::Session,
            "Memory" => Self::Memory,
            _ => Self::Any(name.to_owned()),
        })
    }
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

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TaskMeta {
    pub id: String,
    pub plan_run_id: String,
    #[serde(default)]
    pub origin_plan_id: Option<String>,
    #[serde(default)]
    pub target_plan_id: Option<String>,
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
    pub fn take_payload(&mut self) -> common::AnyType {
        std::mem::replace(&mut self.req, Box::new(()))
    }
    pub fn replace_payload(&mut self, payload: common::AnyType) {
        self.req = payload;
    }
    pub fn payload(&self) -> &common::AnyType {
        &self.req
    }
    pub fn from_parts(ctx: Ctx, meta: TaskMeta, req: common::AnyType) -> Self {
        Self { ctx, meta, req }
    }
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
    tracked: bool,
}
impl TaskResponse {
    pub fn take_payload(&mut self) -> common::AnyType {
        std::mem::replace(&mut self.resp, Box::new(()))
    }
    pub fn replace_payload(&mut self, payload: common::AnyType) {
        self.resp = payload;
    }
    pub fn payload(&self) -> &common::AnyType {
        &self.resp
    }
    pub fn track_completion(&mut self) {
        if self.tracked {
            return;
        }
        self.tracked =
            self.resp.is::<crate::ToolResponse>() || self.resp.is::<crate::ModelResponse>();
        if self.tracked && crate::plugin::leases::has_remote(&self.ctx) {
            return;
        }
        if let Some(tool) = self.resp.downcast_mut::<crate::ToolResponse>() {
            tool.track(self.ctx.clone(), self.meta.clone());
        }
        if let Some(model) = self.resp.downcast_mut::<crate::ModelResponse>() {
            model.track(self.ctx.clone(), self.meta.clone());
        }
    }
    pub(crate) fn mark_completion_tracked(&mut self) {
        self.tracked = true;
    }
    pub(crate) fn new_null() -> Self {
        Self {
            ctx: Ctx::null(),
            meta: TaskMeta::default(),
            resp: Box::new(()),
            tracked: false,
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
            tracked: false,
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
