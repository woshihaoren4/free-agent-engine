pub enum FlowGraphStatus{
    None,
    Running,
    Completed,
    Invalid,
}
pub enum FlowGraphEdgeStatus{
    None,
    Active,
    Invalid,
}

pub struct FlowGraphNode{
    pub id: String,
    pub status: FlowGraphStatus,
    pub from_edgs:Vec<(String,FlowGraphEdgeStatus)>,
    pub to_edgs:Vec<String>,
}

#[derive(Debug,Clone)]
pub struct FlowGraph{
    
}