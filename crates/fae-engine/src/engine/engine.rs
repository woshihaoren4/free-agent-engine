use fae_agent::{PlanBuilder, RT};
use std::collections::HashMap;

#[derive(Debug)]
pub struct Engine {
    plan_generators: HashMap<String, Box<dyn PlanBuilder>>,
    rt: RT,
}
