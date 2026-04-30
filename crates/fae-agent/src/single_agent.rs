use crate::memory::Memory;

pub struct SingleAgentConfig{
    pub module: String,
}

pub struct SingleAgent<T>{
    agent_id: String,
    cfg: SingleAgentConfig,
    memory: Box<dyn Memory<T>+Send+Sync+'static>,
}

