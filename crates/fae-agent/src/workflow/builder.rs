use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use anyhow::Context;
use serde_json::Value;

use super::{
    WORKFLOW_VERSION, WorkflowAction, WorkflowCondition, WorkflowDefinition, WorkflowNode,
};

#[derive(Debug)]
pub struct WorkflowBuilder {
    id: String,
    nodes: BTreeMap<String, WorkflowNode>,
}

impl WorkflowBuilder {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            nodes: BTreeMap::new(),
        }
    }

    pub fn add_node(
        &mut self,
        id: impl Into<String>,
        node: WorkflowNode,
    ) -> anyhow::Result<&mut Self> {
        let id = id.into();
        anyhow::ensure!(!id.trim().is_empty(), "workflow node id cannot be empty");
        anyhow::ensure!(
            !self.nodes.contains_key(&id),
            "workflow node `{id}` already exists"
        );
        self.nodes.insert(id, node);
        Ok(self)
    }

    pub fn start(
        &mut self,
        id: impl Into<String>,
        next: impl Into<String>,
    ) -> anyhow::Result<&mut Self> {
        self.add_node(id, WorkflowNode::Start { next: next.into() })
    }

    pub fn end(
        &mut self,
        id: impl Into<String>,
        output: Option<Value>,
    ) -> anyhow::Result<&mut Self> {
        self.add_node(id, WorkflowNode::End { output })
    }

    pub fn execute(
        &mut self,
        id: impl Into<String>,
        action: WorkflowAction,
        next: impl Into<String>,
    ) -> anyhow::Result<&mut Self> {
        self.add_node(
            id,
            WorkflowNode::Execute {
                action,
                next: next.into(),
            },
        )
    }

    pub fn decision(
        &mut self,
        id: impl Into<String>,
        condition: WorkflowCondition,
        on_true: impl Into<String>,
        on_false: impl Into<String>,
    ) -> anyhow::Result<&mut Self> {
        self.add_node(
            id,
            WorkflowNode::Decision {
                condition,
                on_true: on_true.into(),
                on_false: on_false.into(),
            },
        )
    }

    pub fn loop_node(
        &mut self,
        id: impl Into<String>,
        condition: WorkflowCondition,
        body: impl Into<String>,
        next: impl Into<String>,
        max_iterations: usize,
    ) -> anyhow::Result<&mut Self> {
        self.add_node(
            id,
            WorkflowNode::Loop {
                condition,
                body: body.into(),
                next: next.into(),
                max_iterations,
            },
        )
    }

    pub fn build(self) -> anyhow::Result<WorkflowDefinition> {
        let workflow = WorkflowDefinition {
            version: WORKFLOW_VERSION,
            id: self.id,
            nodes: self.nodes,
        };
        Self::validate_definition(&workflow)?;
        Ok(workflow)
    }

    pub fn validate_definition(workflow: &WorkflowDefinition) -> anyhow::Result<()> {
        anyhow::ensure!(
            workflow.version == WORKFLOW_VERSION,
            "unsupported workflow version {}, expected {}",
            workflow.version,
            WORKFLOW_VERSION
        );
        anyhow::ensure!(
            !workflow.id.trim().is_empty(),
            "workflow id cannot be empty"
        );
        anyhow::ensure!(!workflow.nodes.is_empty(), "workflow has no nodes");

        let starts = workflow
            .nodes
            .iter()
            .filter(|(_, node)| matches!(node, WorkflowNode::Start { .. }))
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>();
        let ends = workflow
            .nodes
            .iter()
            .filter(|(_, node)| matches!(node, WorkflowNode::End { .. }))
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            starts.len() == 1,
            "workflow must contain exactly one start node"
        );
        anyhow::ensure!(
            ends.len() == 1,
            "workflow must contain exactly one end node"
        );
        let start = starts[0];
        let end = ends[0];

        for (id, node) in &workflow.nodes {
            if let WorkflowNode::Loop {
                body,
                next,
                max_iterations,
                ..
            } = node
            {
                anyhow::ensure!(
                    *max_iterations > 0,
                    "loop node `{id}` must allow at least one iteration"
                );
                anyhow::ensure!(
                    body != next,
                    "loop node `{id}` must use different body and exit targets"
                );
            }
            for successor in node.successors() {
                anyhow::ensure!(
                    workflow.nodes.contains_key(successor),
                    "node `{id}` points to missing node `{successor}`"
                );
                anyhow::ensure!(
                    successor != start,
                    "node `{id}` cannot point to start node `{start}`"
                );
            }
        }

        let reachable = collect_reachable(workflow, start);
        anyhow::ensure!(
            reachable.len() == workflow.nodes.len(),
            "workflow contains nodes that are unreachable from start: {}",
            workflow
                .nodes
                .keys()
                .filter(|id| !reachable.contains(id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );

        let can_end = collect_predecessors(workflow, end);
        anyhow::ensure!(
            can_end.len() == workflow.nodes.len(),
            "workflow contains nodes without a path to end: {}",
            workflow
                .nodes
                .keys()
                .filter(|id| !can_end.contains(id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );

        validate_cycles(workflow, start)?;
        for (id, node) in &workflow.nodes {
            if let WorkflowNode::Loop { body, .. } = node {
                anyhow::ensure!(
                    collect_reachable(workflow, body).contains(id.as_str()),
                    "loop node `{id}` body does not return to the loop node"
                );
            }
        }
        Ok(())
    }
}

fn collect_reachable<'a>(workflow: &'a WorkflowDefinition, root: &'a str) -> HashSet<&'a str> {
    let mut found = HashSet::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        if !found.insert(id) {
            continue;
        }
        if let Some(node) = workflow.nodes.get(id) {
            pending.extend(node.successors());
        }
    }
    found
}

fn collect_predecessors<'a>(workflow: &'a WorkflowDefinition, target: &'a str) -> HashSet<&'a str> {
    let mut reverse = HashMap::<&str, Vec<&str>>::new();
    for (id, node) in &workflow.nodes {
        for successor in node.successors() {
            reverse.entry(successor).or_default().push(id);
        }
    }

    let mut found = HashSet::new();
    let mut pending = VecDeque::from([target]);
    while let Some(id) = pending.pop_front() {
        if !found.insert(id) {
            continue;
        }
        pending.extend(reverse.get(id).into_iter().flatten().copied());
    }
    found
}

fn validate_cycles(workflow: &WorkflowDefinition, start: &str) -> anyhow::Result<()> {
    fn visit(
        workflow: &WorkflowDefinition,
        id: &str,
        colors: &mut HashMap<String, u8>,
    ) -> anyhow::Result<()> {
        colors.insert(id.to_string(), 1);
        let node = workflow
            .nodes
            .get(id)
            .with_context(|| format!("workflow node `{id}` disappeared during validation"))?;
        for successor in node.successors() {
            match colors.get(successor).copied().unwrap_or_default() {
                0 => visit(workflow, successor, colors)?,
                1 => anyhow::ensure!(
                    matches!(
                        workflow.nodes.get(successor),
                        Some(WorkflowNode::Loop { .. })
                    ),
                    "cycle from `{id}` to `{successor}` does not return to a loop node"
                ),
                _ => {}
            }
        }
        colors.insert(id.to_string(), 2);
        Ok(())
    }

    visit(workflow, start, &mut HashMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_a_cycle_owned_by_a_loop_node() {
        let mut builder = WorkflowBuilder::new("loop");
        builder.start("start", "loop").unwrap();
        builder
            .loop_node(
                "loop",
                WorkflowCondition::Truthy {
                    value: json!("{$input.keep_running}"),
                },
                "body",
                "end",
                3,
            )
            .unwrap();
        builder
            .execute(
                "body",
                WorkflowAction::Custom {
                    task_type: "test".to_string(),
                    request: Value::Null,
                },
                "loop",
            )
            .unwrap();
        builder.end("end", None).unwrap();

        builder.build().unwrap();
    }

    #[test]
    fn rejects_a_cycle_without_a_loop_node() {
        let mut builder = WorkflowBuilder::new("invalid");
        builder.start("start", "a").unwrap();
        builder
            .execute(
                "a",
                WorkflowAction::Custom {
                    task_type: "test".to_string(),
                    request: Value::Null,
                },
                "b",
            )
            .unwrap();
        builder
            .decision(
                "b",
                WorkflowCondition::Truthy { value: json!(true) },
                "a",
                "end",
            )
            .unwrap();
        builder.end("end", None).unwrap();

        assert!(
            builder
                .build()
                .unwrap_err()
                .to_string()
                .contains("does not return to a loop node")
        );
    }
}
