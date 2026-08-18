use std::collections::HashMap;
use fae_agent::{Event, Runtime};

pub struct EngineRuntime{
    rts: HashMap<String,Box<dyn Runtime>>,
}

// #[async_trait::async_trait]
// impl Runtime for EngineRuntime{
//
// }