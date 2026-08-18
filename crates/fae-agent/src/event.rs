use crate::{common, Plan, TaskResponse};

#[derive(Debug)]
pub enum EventType{
    TaskResult(TaskResponse),
    Plan(Box<dyn Plan>),
    Any(String,common::AnyType)
}

#[derive(Debug)]
pub struct Event{
    pub id: String,
    pub event_type: EventType,
}
