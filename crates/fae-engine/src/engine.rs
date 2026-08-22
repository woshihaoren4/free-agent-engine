use std::collections::HashMap;
use fae_agent::{PlanBuilder, RT};

#[derive(Debug)]
pub struct Engine{
    plan_generators:HashMap<String,Box<dyn PlanBuilder>>,
    rt: RT,
}