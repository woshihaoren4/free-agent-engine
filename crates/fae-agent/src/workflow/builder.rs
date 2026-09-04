use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use anyhow::Context;
use serde_json::Value;

use super::{WORKFLOW_VERSION, WorkflowAction, WorkflowCondition, WorkflowMetadata, WorkflowNode};

pub trait IntoWorkflowTargets {
    fn into_workflow_targets(self) -> Vec<String>;
}

impl IntoWorkflowTargets for String {
    fn into_workflow_targets(self) -> Vec<String> {
        vec![self]
    }
}

impl IntoWorkflowTargets for &str {
    fn into_workflow_targets(self) -> Vec<String> {
        vec![self.to_string()]
    }
}

impl<S, const N: usize> IntoWorkflowTargets for [S; N]
where
    S: Into<String>,
{
    fn into_workflow_targets(self) -> Vec<String> {
        self.into_iter().map(Into::into).collect()
    }
}

impl<S> IntoWorkflowTargets for Vec<S>
where
    S: Into<String>,
{
    fn into_workflow_targets(self) -> Vec<String> {
        self.into_iter().map(Into::into).collect()
    }
}

impl<S> IntoWorkflowTargets for &[S]
where
    S: AsRef<str>,
{
    fn into_workflow_targets(self) -> Vec<String> {
        self.iter()
            .map(|target| target.as_ref().to_string())
            .collect()
    }
}

#[derive(Debug)]
pub struct WorkflowMetadataBuilder {
    id: String,
    nodes: BTreeMap<String, WorkflowNode>,
}

impl WorkflowMetadataBuilder {
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
        next: impl IntoWorkflowTargets,
    ) -> anyhow::Result<&mut Self> {
        self.add_node(
            id,
            WorkflowNode::Start {
                next: next.into_workflow_targets(),
            },
        )
    }

    pub fn start_parallel<I, S>(
        &mut self,
        id: impl Into<String>,
        next: I,
    ) -> anyhow::Result<&mut Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.start(id, next.into_iter().map(Into::into).collect::<Vec<_>>())
    }

    pub fn end(
        &mut self,
        id: impl Into<String>,
        output: Option<Value>,
    ) -> anyhow::Result<&mut Self> {
        self.add_node(id, WorkflowNode::End { output })
    }

    pub fn join_end(
        &mut self,
        id: impl Into<String>,
        output: Option<Value>,
    ) -> anyhow::Result<&mut Self> {
        self.end(id, output)
    }

    pub fn execute(
        &mut self,
        id: impl Into<String>,
        action: WorkflowAction,
        next: impl IntoWorkflowTargets,
    ) -> anyhow::Result<&mut Self> {
        self.add_node(
            id,
            WorkflowNode::Execute {
                action,
                next: next.into_workflow_targets(),
            },
        )
    }

    pub fn decision(
        &mut self,
        id: impl Into<String>,
        condition: WorkflowCondition,
        on_true: impl IntoWorkflowTargets,
        on_false: impl IntoWorkflowTargets,
    ) -> anyhow::Result<&mut Self> {
        self.add_node(
            id,
            WorkflowNode::Decision {
                condition,
                on_true: on_true.into_workflow_targets(),
                on_false: on_false.into_workflow_targets(),
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

    pub fn build(self) -> anyhow::Result<WorkflowMetadata> {
        let metadata = WorkflowMetadata {
            version: WORKFLOW_VERSION,
            id: self.id,
            nodes: self.nodes,
        };
        Self::validate_metadata(&metadata)?;
        Ok(metadata)
    }

    pub fn validate_metadata(metadata: &WorkflowMetadata) -> anyhow::Result<()> {
        anyhow::ensure!(
            metadata.version == WORKFLOW_VERSION,
            "unsupported workflow version {}, expected {}",
            metadata.version,
            WORKFLOW_VERSION
        );
        anyhow::ensure!(
            !metadata.id.trim().is_empty(),
            "workflow id cannot be empty"
        );
        anyhow::ensure!(!metadata.nodes.is_empty(), "workflow has no nodes");

        let starts = metadata
            .nodes
            .iter()
            .filter(|(_, node)| {
                matches!(
                    node,
                    WorkflowNode::Start { .. } | WorkflowNode::ParallelStart { .. }
                )
            })
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>();
        let ends = metadata
            .nodes
            .iter()
            .filter(|(_, node)| {
                matches!(
                    node,
                    WorkflowNode::End { .. } | WorkflowNode::JoinEnd { .. }
                )
            })
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

        for (id, node) in &metadata.nodes {
            match node {
                WorkflowNode::Start { next }
                | WorkflowNode::ParallelStart { next }
                | WorkflowNode::Execute { next, .. } => {
                    validate_targets(id, "next", next)?;
                }
                WorkflowNode::Decision {
                    on_true, on_false, ..
                } => {
                    validate_targets(id, "on_true", on_true)?;
                    validate_targets(id, "on_false", on_false)?;
                }
                _ => {}
            }
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
                    metadata.nodes.contains_key(successor),
                    "node `{id}` points to missing node `{successor}`"
                );
                anyhow::ensure!(
                    successor != start,
                    "node `{id}` cannot point to start node `{start}`"
                );
            }
        }

        if requires_dag_execution(metadata) {
            anyhow::ensure!(
                !metadata
                    .nodes
                    .values()
                    .any(|node| matches!(node, WorkflowNode::Loop { .. })),
                "loop nodes cannot be combined with fan-out or multi-input joins"
            );
        }

        let reachable = collect_reachable(metadata, start);
        anyhow::ensure!(
            reachable.len() == metadata.nodes.len(),
            "workflow contains nodes that are unreachable from start: {}",
            metadata
                .nodes
                .keys()
                .filter(|id| !reachable.contains(id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );

        let can_end = collect_predecessors(metadata, end);
        anyhow::ensure!(
            can_end.len() == metadata.nodes.len(),
            "workflow contains nodes without a path to end: {}",
            metadata
                .nodes
                .keys()
                .filter(|id| !can_end.contains(id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );

        validate_cycles(metadata, start)?;
        for (id, node) in &metadata.nodes {
            if let WorkflowNode::Loop { body, .. } = node {
                anyhow::ensure!(
                    collect_reachable(metadata, body).contains(id.as_str()),
                    "loop node `{id}` body does not return to the loop node"
                );
            }
        }
        Ok(())
    }
}

fn validate_targets(id: &str, field: &str, targets: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        !targets.is_empty(),
        "workflow node `{id}` field `{field}` must contain at least one target"
    );
    anyhow::ensure!(
        targets.iter().collect::<HashSet<_>>().len() == targets.len(),
        "workflow node `{id}` field `{field}` contains duplicate targets"
    );
    Ok(())
}

pub(super) fn requires_dag_execution(metadata: &WorkflowMetadata) -> bool {
    if metadata.nodes.values().any(|node| match node {
        WorkflowNode::ParallelStart { .. } | WorkflowNode::JoinEnd { .. } => true,
        WorkflowNode::Start { next } | WorkflowNode::Execute { next, .. } => next.len() > 1,
        WorkflowNode::Decision {
            on_true, on_false, ..
        } => on_true.len() > 1 || on_false.len() > 1,
        WorkflowNode::End { .. } | WorkflowNode::Loop { .. } => false,
    }) {
        return true;
    }

    let mut predecessors = HashMap::<&str, HashSet<&str>>::new();
    for (id, node) in &metadata.nodes {
        for successor in node.successors() {
            if matches!(
                metadata.nodes.get(successor),
                Some(WorkflowNode::Loop { .. })
            ) {
                continue;
            }
            predecessors.entry(successor).or_default().insert(id);
        }
    }
    predecessors.values().any(|incoming| incoming.len() > 1)
}

fn collect_reachable<'a>(metadata: &'a WorkflowMetadata, root: &'a str) -> HashSet<&'a str> {
    let mut found = HashSet::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        if !found.insert(id) {
            continue;
        }
        if let Some(node) = metadata.nodes.get(id) {
            pending.extend(node.successors());
        }
    }
    found
}

fn collect_predecessors<'a>(metadata: &'a WorkflowMetadata, target: &'a str) -> HashSet<&'a str> {
    let mut reverse = HashMap::<&str, Vec<&str>>::new();
    for (id, node) in &metadata.nodes {
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

fn validate_cycles(metadata: &WorkflowMetadata, start: &str) -> anyhow::Result<()> {
    fn visit(
        metadata: &WorkflowMetadata,
        id: &str,
        colors: &mut HashMap<String, u8>,
    ) -> anyhow::Result<()> {
        colors.insert(id.to_string(), 1);
        let node = metadata
            .nodes
            .get(id)
            .with_context(|| format!("workflow node `{id}` disappeared during validation"))?;
        for successor in node.successors() {
            match colors.get(successor).copied().unwrap_or_default() {
                0 => visit(metadata, successor, colors)?,
                1 => anyhow::ensure!(
                    matches!(
                        metadata.nodes.get(successor),
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

    visit(metadata, start, &mut HashMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_a_cycle_owned_by_a_loop_node() {
        let mut builder = WorkflowMetadataBuilder::new("loop");
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
        let mut builder = WorkflowMetadataBuilder::new("invalid");
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

    #[test]
    fn accepts_parallel_branches_that_merge_at_a_regular_node() {
        let mut builder = WorkflowMetadataBuilder::new("parallel");
        builder.start("a", ["b", "c"]).unwrap();
        builder
            .execute(
                "b",
                WorkflowAction::Custom {
                    task_type: "test".to_string(),
                    request: Value::Null,
                },
                "shared",
            )
            .unwrap();
        builder
            .execute(
                "c",
                WorkflowAction::Custom {
                    task_type: "test".to_string(),
                    request: Value::Null,
                },
                "shared",
            )
            .unwrap();
        builder
            .execute(
                "shared",
                WorkflowAction::Custom {
                    task_type: "test".to_string(),
                    request: Value::Null,
                },
                "e",
            )
            .unwrap();
        builder.end("e", None).unwrap();

        builder.build().unwrap();
    }
}
