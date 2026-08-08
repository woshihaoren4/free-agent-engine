use std::collections::HashMap;
use fae_agent::{PlanGenerator, RT};

#[derive(Debug)]
pub struct Engine{
    plan_generators:HashMap<String,Box<dyn PlanGenerator>>,
    rt: RT,
}