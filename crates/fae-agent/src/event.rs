use crate::{common, TaskRequest, TaskResponse};

#[derive(Debug)]
pub enum EventType{
    Task(TaskRequest),
    TaskResult(TaskResponse),
    Any(String,common::AnyType)
}

#[derive(Debug)]
pub struct Event{
    pub from_rt_id: String,
    pub event_type: EventType,
}
