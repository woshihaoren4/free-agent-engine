use crate::{common, TaskResult};

#[derive(Debug)]
pub enum EventType{
    TaskResult(TaskResult),
    Any(String,common::AnyType)
}

#[derive(Debug)]
pub struct Event{
    pub id: String,
    pub event_type: EventType,
}
