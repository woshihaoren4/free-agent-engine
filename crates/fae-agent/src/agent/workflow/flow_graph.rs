use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowGraphStatus {
    None,
    Running,
    Completed,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowGraphEdgeStatus {
    None,
    Active,
    Invalid,
}

#[derive(Debug, Clone)]
pub struct FlowGraphNode {
    pub id: String,
    // 最后一次允许的状态
    pub status: FlowGraphStatus,
    // 运行次数
    pub run_count: u32,
    pub from_edgs: Vec<(String, FlowGraphEdgeStatus)>,
    pub to_edgs: Vec<String>,
}

impl FlowGraphNode {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: FlowGraphStatus::None,
            run_count: 0,
            from_edgs: Vec::new(),
            to_edgs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FlowGraph {
    pub is_over: bool,
    pub graph: HashMap<String, FlowGraphNode>,
    pub start_node_id: String,
    pub end_node_id: String,
}

impl FlowGraph {
    pub fn new(start_node_id: impl Into<String>, end_node_id: impl Into<String>) -> Self {
        let start_node_id = start_node_id.into();
        let end_node_id = end_node_id.into();
        let mut graph = HashMap::new();
        graph.insert(
            start_node_id.clone(),
            FlowGraphNode::new(start_node_id.clone()),
        );
        graph.insert(end_node_id.clone(), FlowGraphNode::new(end_node_id.clone()));

        Self {
            is_over: false,
            graph,
            start_node_id,
            end_node_id,
        }
    }

    // fn 查询状态
    pub fn node_status(&self, node_id: &str) -> Option<FlowGraphStatus> {
        self.graph.get(node_id).map(|node| node.status)
    }

    pub fn edge_status(&self, from_node_id: &str, to_node_id: &str) -> Option<FlowGraphEdgeStatus> {
        self.graph
            .get(to_node_id)?
            .from_edgs
            .iter()
            .find_map(|(from_id, status)| (from_id == from_node_id).then_some(*status))
    }

    // fn 添加节点
    pub fn add_node(&mut self, node_id: impl Into<String>) -> anyhow::Result<()> {
        let node_id = node_id.into();
        if self.graph.contains_key(&node_id) {
            anyhow::bail!("flow graph node `{}` already exists", node_id);
        }

        self.graph
            .insert(node_id.clone(), FlowGraphNode::new(node_id));
        Ok(())
    }

    // fn 添加边
    pub fn add_edge(&mut self, from_node_id: &str, to_node_id: &str) -> anyhow::Result<()> {
        if !self.graph.contains_key(from_node_id) {
            anyhow::bail!("flow graph source node `{}` does not exist", from_node_id);
        }
        if !self.graph.contains_key(to_node_id) {
            anyhow::bail!("flow graph target node `{}` does not exist", to_node_id);
        }
        if self
            .graph
            .get(from_node_id)
            .is_some_and(|node| node.to_edgs.iter().any(|id| id == to_node_id))
        {
            anyhow::bail!(
                "flow graph edge `{} -> {}` already exists",
                from_node_id,
                to_node_id
            );
        }

        self.graph
            .get_mut(from_node_id)
            .expect("source node existence checked")
            .to_edgs
            .push(to_node_id.to_string());
        self.graph
            .get_mut(to_node_id)
            .expect("target node existence checked")
            .from_edgs
            .push((from_node_id.to_string(), FlowGraphEdgeStatus::None));
        Ok(())
    }

    // fn检查流程图是否正确。允许有环，允许有判断结构
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.graph.is_empty() {
            anyhow::bail!("flow graph is empty");
        }
        if !self.graph.contains_key(&self.start_node_id) {
            anyhow::bail!(
                "flow graph start node `{}` does not exist",
                self.start_node_id
            );
        }
        if !self.graph.contains_key(&self.end_node_id) {
            anyhow::bail!("flow graph end node `{}` does not exist", self.end_node_id);
        }
        if self.start_node_id == self.end_node_id {
            anyhow::bail!("flow graph start node and end node must be different");
        }

        let start_nodes = self
            .graph
            .values()
            .filter(|node| node.from_edgs.is_empty())
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>();
        if start_nodes != [self.start_node_id.as_str()] {
            anyhow::bail!(
                "flow graph must have exactly one start node `{}`, actual: {:?}",
                self.start_node_id,
                start_nodes
            );
        }

        let end_nodes = self
            .graph
            .values()
            .filter(|node| node.to_edgs.is_empty())
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>();
        if end_nodes != [self.end_node_id.as_str()] {
            anyhow::bail!(
                "flow graph must have exactly one end node `{}`, actual: {:?}",
                self.end_node_id,
                end_nodes
            );
        }

        for node in self.graph.values() {
            for next_id in &node.to_edgs {
                let Some(next_node) = self.graph.get(next_id) else {
                    anyhow::bail!(
                        "flow graph edge `{} -> {}` points to a missing node",
                        node.id,
                        next_id
                    );
                };
                if !next_node
                    .from_edgs
                    .iter()
                    .any(|(from_id, _)| from_id == &node.id)
                {
                    anyhow::bail!(
                        "flow graph edge `{} -> {}` is missing reverse metadata",
                        node.id,
                        next_id
                    );
                }
            }
        }

        let reachable_from_start = self.forward_reachable(&self.start_node_id);
        if reachable_from_start.len() != self.graph.len() {
            let unreachable = self
                .graph
                .keys()
                .filter(|id| !reachable_from_start.contains(*id))
                .cloned()
                .collect::<Vec<_>>();
            anyhow::bail!(
                "flow graph contains nodes unreachable from start `{}`: {:?}",
                self.start_node_id,
                unreachable
            );
        }

        let can_reach_end = self.backward_reachable(&self.end_node_id);
        if can_reach_end.len() != self.graph.len() {
            let dead_nodes = self
                .graph
                .keys()
                .filter(|id| !can_reach_end.contains(*id))
                .cloned()
                .collect::<Vec<_>>();
            anyhow::bail!(
                "flow graph contains nodes that cannot reach end `{}`: {:?}",
                self.end_node_id,
                dead_nodes
            );
        }

        Ok(())
    }

    pub fn start(&mut self) -> anyhow::Result<Vec<String>> {
        self.validate()?;
        self.reset();
        if self.start_node_id == self.end_node_id {
            self.is_over = true;
            return Ok(Vec::new());
        }

        self.graph
            .get_mut(&self.start_node_id)
            .expect("start node existence checked")
            .status = FlowGraphStatus::Running;
        Ok(vec![self.start_node_id.clone()])
    }

    pub fn reset(&mut self) {
        self.is_over = false;
        for node in self.graph.values_mut() {
            node.status = FlowGraphStatus::None;
            node.run_count = 0;
            for (_, status) in &mut node.from_edgs {
                *status = FlowGraphEdgeStatus::None;
            }
        }
    }

    // fn 节点完成运行, 给出下一步要执行的节点
    pub fn node_complete<F>(
        &mut self,
        node_id: &str,
        handle: Option<F>,
    ) -> anyhow::Result<Vec<String>>
    where
        F: FnOnce(&mut FlowGraphNode) -> anyhow::Result<Vec<(String, FlowGraphEdgeStatus)>>,
    {
        if self.is_over {
            return Ok(Vec::new());
        }

        //修改节点状态等逻辑
        let to_edgs = {
            let node = self
                .graph
                .get_mut(node_id)
                .ok_or_else(|| anyhow::anyhow!("flow graph node `{}` does not exist", node_id))?;

            if node.status != FlowGraphStatus::Running {
                anyhow::bail!(
                    "flow graph node `{}` cannot complete from status {:?}",
                    node_id,
                    node.status
                );
            }

            node.run_count = node.run_count.checked_add(1).ok_or_else(|| {
                anyhow::anyhow!("flow graph node `{}` run count overflow", node_id)
            })?;

            let next_edges = if let Some(handle) = handle {
                //如果handle不是空，则调用handle，得到要向下传递的边和状态
                handle(node)?
            } else {
                //如果handle是空，则向下传递to_edgs，状态为Active
                node.to_edgs
                    .iter()
                    .map(|next_id| (next_id.clone(), FlowGraphEdgeStatus::Active))
                    .collect::<Vec<_>>()
            };

            if node.status != FlowGraphStatus::Invalid {
                node.status = FlowGraphStatus::Completed;
            }

            next_edges
        };

        for (to_node_id, edge_status) in to_edgs {
            self.set_edge_status(node_id, &to_node_id, edge_status)?;

            if edge_status == FlowGraphEdgeStatus::Active {
                if to_node_id == self.end_node_id {
                    self.is_over = true;
                    if let Some(end_node) = self.graph.get_mut(&to_node_id) {
                        end_node.status = FlowGraphStatus::Completed;
                    }
                } else if let Some(to_node) = self.graph.get_mut(&to_node_id) {
                    if to_node.status != FlowGraphStatus::Invalid {
                        to_node.status = FlowGraphStatus::Running;
                    }
                }
            }
        }

        //如果handle是空，则向下传递to_edgs，状态为Active
        //如果handle不是空，则调用handle，得到要向下传递的边和状态
        //找到所有running的节点并返回
        Ok(self.running_nodes())
    }

    fn running_nodes(&self) -> Vec<String> {
        let mut nodes = self
            .graph
            .values()
            .filter(|node| node.status == FlowGraphStatus::Running)
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        nodes.sort();
        nodes
    }

    fn set_edge_status(
        &mut self,
        from_node_id: &str,
        to_node_id: &str,
        edge_status: FlowGraphEdgeStatus,
    ) -> anyhow::Result<()> {
        let from_node = self.graph.get(from_node_id).ok_or_else(|| {
            anyhow::anyhow!("flow graph source node `{}` does not exist", from_node_id)
        })?;
        if !from_node.to_edgs.iter().any(|id| id == to_node_id) {
            anyhow::bail!(
                "flow graph edge `{} -> {}` does not exist",
                from_node_id,
                to_node_id
            );
        }

        let to_node = self.graph.get_mut(to_node_id).ok_or_else(|| {
            anyhow::anyhow!("flow graph target node `{}` does not exist", to_node_id)
        })?;
        let Some((_, status)) = to_node
            .from_edgs
            .iter_mut()
            .find(|(from_id, _)| from_id == from_node_id)
        else {
            anyhow::bail!(
                "flow graph edge `{} -> {}` is missing reverse metadata",
                from_node_id,
                to_node_id
            );
        };

        *status = edge_status;
        Ok(())
    }

    fn forward_reachable(&self, start_node_id: &str) -> HashSet<String> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::from([start_node_id.to_string()]);

        while let Some(node_id) = queue.pop_front() {
            if !visited.insert(node_id.clone()) {
                continue;
            }

            if let Some(node) = self.graph.get(&node_id) {
                queue.extend(node.to_edgs.iter().cloned());
            }
        }

        visited
    }

    fn backward_reachable(&self, end_node_id: &str) -> HashSet<String> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::from([end_node_id.to_string()]);

        while let Some(node_id) = queue.pop_front() {
            if !visited.insert(node_id.clone()) {
                continue;
            }

            if let Some(node) = self.graph.get(&node_id) {
                queue.extend(node.from_edgs.iter().map(|(from_id, _)| from_id.clone()));
            }
        }

        visited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Handler = fn(&mut FlowGraphNode) -> anyhow::Result<Vec<(String, FlowGraphEdgeStatus)>>;

    fn select_a(_: &mut FlowGraphNode) -> anyhow::Result<Vec<(String, FlowGraphEdgeStatus)>> {
        Ok(vec![
            ("a".to_string(), FlowGraphEdgeStatus::Active),
            ("b".to_string(), FlowGraphEdgeStatus::Invalid),
        ])
    }

    fn select_only_a(_: &mut FlowGraphNode) -> anyhow::Result<Vec<(String, FlowGraphEdgeStatus)>> {
        Ok(vec![("a".to_string(), FlowGraphEdgeStatus::Active)])
    }

    fn select_loop(_: &mut FlowGraphNode) -> anyhow::Result<Vec<(String, FlowGraphEdgeStatus)>> {
        Ok(vec![("loop".to_string(), FlowGraphEdgeStatus::Active)])
    }

    fn graph_with_branch() -> FlowGraph {
        let mut graph = FlowGraph::new("start", "end");
        graph.add_node("a").unwrap();
        graph.add_node("b").unwrap();
        graph.add_edge("start", "a").unwrap();
        graph.add_edge("start", "b").unwrap();
        graph.add_edge("a", "end").unwrap();
        graph.add_edge("b", "end").unwrap();
        graph
    }

    #[test]
    fn validate_accepts_branch_graph() {
        let graph = graph_with_branch();

        graph.validate().unwrap();
    }

    #[test]
    fn validate_rejects_extra_start_node() {
        let mut graph = graph_with_branch();
        graph.add_node("orphan").unwrap();
        graph.add_edge("orphan", "end").unwrap();

        assert!(graph.validate().is_err());
    }

    #[test]
    fn start_sets_start_node_running() {
        let mut graph = graph_with_branch();

        assert_eq!(graph.start().unwrap(), vec!["start"]);
        assert_eq!(graph.node_status("start"), Some(FlowGraphStatus::Running));
    }

    #[test]
    fn node_complete_without_handler_activates_all_next_nodes() {
        let mut graph = graph_with_branch();
        graph.start().unwrap();

        let running = graph
            .node_complete("start", Option::<Handler>::None)
            .unwrap();

        assert_eq!(running, vec!["a", "b"]);
        assert_eq!(graph.node_status("start"), Some(FlowGraphStatus::Completed));
        assert_eq!(
            graph.edge_status("start", "a"),
            Some(FlowGraphEdgeStatus::Active)
        );
        assert_eq!(
            graph.edge_status("start", "b"),
            Some(FlowGraphEdgeStatus::Active)
        );
    }

    #[test]
    fn node_complete_with_handler_can_select_branch() {
        let mut graph = graph_with_branch();
        graph.start().unwrap();

        let running = graph
            .node_complete("start", Some(select_a as Handler))
            .unwrap();

        assert_eq!(running, vec!["a"]);
        assert_eq!(
            graph.edge_status("start", "a"),
            Some(FlowGraphEdgeStatus::Active)
        );
        assert_eq!(
            graph.edge_status("start", "b"),
            Some(FlowGraphEdgeStatus::Invalid)
        );
    }

    #[test]
    fn active_end_edge_marks_graph_over() {
        let mut graph = graph_with_branch();
        graph.start().unwrap();
        graph
            .node_complete("start", Some(select_only_a as Handler))
            .unwrap();

        let running = graph.node_complete("a", Option::<Handler>::None).unwrap();

        assert!(running.is_empty());
        assert!(graph.is_over);
        assert_eq!(graph.node_status("end"), Some(FlowGraphStatus::Completed));
    }

    #[test]
    fn graph_can_reactivate_completed_node_through_cycle() {
        let mut graph = FlowGraph::new("start", "end");
        graph.add_node("loop").unwrap();
        graph.add_edge("start", "loop").unwrap();
        graph.add_edge("loop", "loop").unwrap();
        graph.add_edge("loop", "end").unwrap();
        graph.start().unwrap();
        graph
            .node_complete("start", Option::<Handler>::None)
            .unwrap();

        let running = graph
            .node_complete("loop", Some(select_loop as Handler))
            .unwrap();

        assert_eq!(running, vec!["loop"]);
        assert_eq!(graph.node_status("loop"), Some(FlowGraphStatus::Running));
        assert_eq!(graph.graph["loop"].run_count, 1);
    }
}
