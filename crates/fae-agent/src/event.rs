use crate::TaskResult;

#[derive(Debug)]
pub enum EventType{
    TaskResult(TaskResult),
}

#[derive(Debug)]
pub struct Event{
    pub id: String,
    pub event_type: EventType,
}
