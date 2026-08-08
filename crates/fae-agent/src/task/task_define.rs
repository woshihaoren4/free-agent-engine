use std::any::Any;
use std::fmt::Debug;
use serde::{Deserialize, Serialize};
use crate::common;

#[derive(Debug, Clone,PartialEq,Eq,Serialize,Deserialize)]
pub enum TaskType{
    Plan,
    Tool,
    Any(String),
}

pub trait IntoTaskRequest{
    fn into_task_request(self) -> common::AnyType;
}
impl<T> IntoTaskRequest for T where T:Any+Send+Sync+'static{
    fn into_task_request(self) -> common::AnyType {
        Box::new(self)
    }
}

#[derive(Debug)]
pub struct Task{
    pub id: String,
    pub ty:TaskType,
    pub author: String,
    req: common::AnyType
}

impl Task {
    pub fn new<T: IntoTaskRequest>(id: String, ty: TaskType, author: String, req: T) -> Self {
        Self {
            id,
            ty,
            author,
            req: req.into_task_request(),
        }
    }
}

#[derive(Debug)]
pub struct TaskResult{
    pub id: String,
    pub ty:TaskType,
    pub consumer: String,
    result: common::AnyType
}
