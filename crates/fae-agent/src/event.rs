use crate::{Ctx, TaskMeta, TaskRequest, TaskResponse, common};

#[derive(Debug)]
pub struct TaskError {
    pub ctx: Ctx,
    pub meta: TaskMeta,
    pub error: String,
}

#[derive(Debug)]
pub enum EventType {
    Task(TaskRequest),
    TaskResult(TaskResponse),
    TaskError(TaskError),
    Any(String, common::AnyType),
}

#[derive(Debug)]
pub struct Event {
    pub from_rt_id: String,
    pub event_type: EventType,
}
