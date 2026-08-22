use std::collections::HashMap;
use fae_agent::{PlanBuilder, Runtime, TaskType};

#[derive(Debug)]
pub struct EngineBuilder {
    plan_builders:HashMap<String,Box<dyn PlanBuilder>>,
    runtimes: HashMap<TaskType,Box<dyn Runtime>>,

}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self{
            plan_builders: HashMap::new(),
            runtimes: HashMap::new(),
        }
    }
}

impl EngineBuilder {

}