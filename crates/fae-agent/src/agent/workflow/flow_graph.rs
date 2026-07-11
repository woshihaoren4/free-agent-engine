#[derive(Debug,Clone)]
pub enum FlowGraphStatus{
    None,
    Running,
    Completed,
    Invalid,
}
#[derive(Debug,Clone)]
pub enum FlowGraphEdgeStatus{
    None,
    Active,
    Invalid,
}

#[derive(Debug,Clone)]
pub struct FlowGraphNode{
    pub id: String,
    // 最后一次允许的状态
    pub status: FlowGraphStatus,
    // 运行次数
    pub run_count: u32,
    pub from_edgs:Vec<(String,FlowGraphEdgeStatus)>,
    pub to_edgs:Vec<String>,
}

#[derive(Debug,Clone,Default)]
pub struct FlowGraph{
    pub is_over: bool,
    pub graph: HashMap<String,FlowGraphNode>,
    pub start_node_id: String,
    pub end_node_id: String,
}

impl FlowGraph{
    // fn 查询状态
    // fn 添加节点
    // fn 添加边
    // fn检查流程图是否正确。允许有环，允许有判断结构
    // fn 节点完成运行, 给出下一步要执行的节点
    pub fn node_complete(&mut self, node_id: &str,handle:Option<impl FnOnce(&mut FlowGraphNode) -> anyhow::Result<Vec<(String,FlowGraphEdgeStatus)>>>)->anyhow::Result<Vec<String>>{
        //修改节点状态等逻辑
        //如果handle是空，则向下传递to_edgs，状态为Active
        //如果handle不是空，则调用handle，得到要向下传递的边和状态
        //找到所有running的节点并返回
        todo!()
    }
}