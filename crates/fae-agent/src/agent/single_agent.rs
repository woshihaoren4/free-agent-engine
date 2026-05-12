use std::sync::Arc;
use crate::memory::Memory;

pub struct SingleAgentConfig{
    pub module: String,
}

pub struct SingleAgent<M,S>{
    agent_id: String,
    cfg: SingleAgentConfig,
    memory: Arc<dyn Memory<M>+Send+Sync+'static>,
    session_manager: Arc<dyn SessionMetaManager<S>+Send+Sync+'static>,
}

