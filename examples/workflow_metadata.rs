use std::collections::HashMap;

use fae_agent::{
    SingleAgentInfo, SingleAgentModelConfig, WorkflowAction, WorkflowCompare, WorkflowCondition,
    WorkflowMetadata, WorkflowMetadataBuilder,
};
use fae_engine::{READ_FILE, WRITE_FILE};
use serde_json::{Value, json};

pub const PYTHON_ACTION_TASK_TYPE: &str = "workflow.python";

pub fn build_release_review_workflow(
    input: Value,
    model: SingleAgentModelConfig,
) -> anyhow::Result<WorkflowMetadata> {
    let mut builder = WorkflowMetadataBuilder::new("release-readiness-review");
    builder.input(input);

    builder.start("start", "select_policy")?;
    builder.decision(
        "select_policy",
        WorkflowCondition::Compare {
            left: json!("{$input.policy}"),
            op: WorkflowCompare::Eq,
            right: json!("strict"),
        },
        "strict_policy",
        "quick_policy",
    )?;
    builder.execute(
        "strict_policy",
        python_action(
            r#"result = {"policy": "strict", "checks": ["source", "manifest"]}"#,
            Value::Null,
        ),
        "dispatch_checks",
    )?;
    builder.execute(
        "quick_policy",
        python_action(
            r#"result = {"policy": "quick", "checks": ["source", "manifest"]}"#,
            Value::Null,
        ),
        "dispatch_checks",
    )?;
    builder.execute(
        "dispatch_checks",
        python_action(
            r#"result = {"policy": arguments["policy"], "parallel_checks": 2}"#,
            json!({"policy": "{$input.policy}"}),
        ),
        ["read_source", "read_manifest"],
    )?;

    builder.execute(
        "read_source",
        WorkflowAction::Tool {
            tool_name: READ_FILE.to_string(),
            arguments: json!({
                "path": "{$input.source_path}",
                "max_bytes": 24 * 1024
            }),
        },
        "review_source",
    )?;
    builder.execute(
        "read_manifest",
        WorkflowAction::Tool {
            tool_name: READ_FILE.to_string(),
            arguments: json!({
                "path": "{$input.manifest_path}",
                "max_bytes": 12 * 1024
            }),
        },
        "review_manifest",
    )?;
    builder.execute(
        "review_source",
        agent_action(
            "source-reviewer",
            concat!(
                "Review the Rust source for release risks. Return concise JSON with keys ",
                "`summary`, `risks`, and `recommendation`."
            ),
            json!("Policy: {$input.policy}\nPath: {$read_source.path}\n\n{$read_source.content}"),
            model.clone(),
        ),
        "aggregate_reviews",
    )?;
    builder.execute(
        "review_manifest",
        agent_action(
            "manifest-reviewer",
            concat!(
                "Review this Cargo manifest for release risks. Return concise JSON with keys ",
                "`summary`, `risks`, and `recommendation`."
            ),
            json!(
                "Policy: {$input.policy}\nPath: {$read_manifest.path}\n\n{$read_manifest.content}"
            ),
            model.clone(),
        ),
        "aggregate_reviews",
    )?;
    builder.execute(
        "aggregate_reviews",
        python_action(
            concat!(
                "result = {\n",
                "    \"approved\": not arguments[\"run_remediation\"],\n",
                "    \"source_review\": arguments[\"source_review\"],\n",
                "    \"manifest_review\": arguments[\"manifest_review\"],\n",
                "    \"review_count\": 2,\n",
                "}"
            ),
            json!({
                "run_remediation": "{$input.run_remediation}",
                "source_review": "{$review_source}",
                "manifest_review": "{$review_manifest}"
            }),
        ),
        "route_remediation",
    )?;
    builder.decision(
        "route_remediation",
        WorkflowCondition::Compare {
            left: json!("{$aggregate_reviews.approved}"),
            op: WorkflowCompare::Eq,
            right: json!(false),
        },
        "remediation",
        "skip_remediation",
    )?;
    builder.execute(
        "remediation",
        WorkflowAction::Workflow {
            workflow: Box::new(build_remediation_workflow()?),
        },
        "final_report",
    )?;
    builder.execute(
        "skip_remediation",
        python_action(
            r#"result = {"status": "skipped", "reason": "reviews approved"}"#,
            Value::Null,
        ),
        "final_report",
    )?;
    builder.execute(
        "final_report",
        agent_action(
            "release-manager",
            concat!(
                "Produce a concise final release-readiness report from the supplied review ",
                "bundle. State the policy, decision, and the most important next action."
            ),
            json!({
                "policy": "{$input.policy}",
                "quality_gate": "{$aggregate_reviews}",
                "remediation_requested": "{$input.run_remediation}"
            }),
            model,
        ),
        "end",
    )?;
    builder.end(
        "end",
        Some(json!({
            "policy": "{$input.policy}",
            "quality_gate": "{$aggregate_reviews}",
            "report": "{$final_report}"
        })),
    )?;

    builder.build()
}

pub fn build_remediation_workflow() -> anyhow::Result<WorkflowMetadata> {
    let mut builder = WorkflowMetadataBuilder::new("bounded-remediation-loop");
    builder.input(json!({
        "counter_path": "{$input.counter_path}",
        "rounds": "{$input.remediation_rounds}"
    }));

    builder.start("start", "seed_counter")?;
    builder.execute(
        "seed_counter",
        WorkflowAction::Tool {
            tool_name: WRITE_FILE.to_string(),
            arguments: json!({
                "path": "{$input.counter_path}",
                "content": "remaining={$input.rounds}",
                "create_parent": true
            }),
        },
        "initialize_state",
    )?;
    builder.execute(
        "initialize_state",
        python_action(
            r#"result = {"remaining": arguments["rounds"]}"#,
            json!({"rounds": "{$input.rounds}"}),
        ),
        "retry_loop",
    )?;
    builder.loop_node(
        "retry_loop",
        WorkflowCondition::Compare {
            left: json!("{$last.remaining}"),
            op: WorkflowCompare::Gt,
            right: json!(0),
        },
        "decrement_counter",
        "done",
        8,
    )?;
    builder.execute(
        "decrement_counter",
        python_action(
            concat!(
                "remaining = arguments[\"remaining\"] - 1\n",
                "result = {\"remaining\": remaining, \"completed_round\": ",
                "arguments[\"iteration\"]}"
            ),
            json!({
                "remaining": "{$last.remaining}",
                "iteration": "{$loop.retry_loop.iteration}"
            }),
        ),
        "retry_loop",
    )?;
    builder.end(
        "done",
        Some(json!({
            "status": "completed",
            "iterations": "{$loop.retry_loop.iteration}",
            "remaining": "{$last.remaining}"
        })),
    )?;

    builder.build()
}

fn python_action(code: impl Into<String>, arguments: Value) -> WorkflowAction {
    WorkflowAction::Python {
        code: code.into(),
        arguments,
        task_type: PYTHON_ACTION_TASK_TYPE.to_string(),
    }
}

fn agent_action(
    name: &str,
    prompt: &str,
    input: Value,
    model: SingleAgentModelConfig,
) -> WorkflowAction {
    WorkflowAction::SingleAgent {
        agent: SingleAgentInfo {
            name: name.to_string(),
            user_id: "workflow-example".to_string(),
            session_id: format!("workflow-example-{name}"),
            metadata: HashMap::new(),
        },
        prompt: prompt.to_string(),
        model,
        input,
        tools: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::WorkflowNode;

    fn model() -> SingleAgentModelConfig {
        SingleAgentModelConfig {
            model: "test-model".to_string(),
            context_size: 8_192,
            history_turns: 1,
            max_completion_tokens: Some(512),
            temperature: Some(0.0),
            max_tool_iterations: 1,
        }
    }

    #[test]
    fn metadata_covers_parallel_choice_loop_tools_agents_and_python() {
        let workflow = build_release_review_workflow(
            json!({
                "policy": "strict",
                "run_remediation": true,
                "source_path": "src/lib.rs",
                "manifest_path": "Cargo.toml",
                "counter_path": "target/workflow-counter.txt",
                "remediation_rounds": 2
            }),
            model(),
        )
        .unwrap();

        assert!(matches!(
            workflow.nodes["select_policy"],
            WorkflowNode::Decision { .. }
        ));
        assert!(matches!(
            workflow.nodes["dispatch_checks"],
            WorkflowNode::Execute { ref next, .. } if next.len() == 2
        ));
        assert!(workflow.nodes.values().any(|node| matches!(
            node,
            WorkflowNode::Execute {
                action: WorkflowAction::Tool { .. },
                ..
            }
        )));
        assert!(workflow.nodes.values().any(|node| matches!(
            node,
            WorkflowNode::Execute {
                action: WorkflowAction::SingleAgent { .. },
                ..
            }
        )));
        assert!(workflow.nodes.values().any(|node| matches!(
            node,
            WorkflowNode::Execute {
                action: WorkflowAction::Python { .. },
                ..
            }
        )));

        let WorkflowNode::Execute {
            action: WorkflowAction::Workflow { workflow: child },
            ..
        } = &workflow.nodes["remediation"]
        else {
            panic!("remediation must execute a nested workflow");
        };
        assert!(
            child
                .nodes
                .values()
                .any(|node| matches!(node, WorkflowNode::Loop { .. }))
        );
        assert!(WorkflowMetadata::from_json(&workflow.to_json().unwrap()).is_ok());
    }
}
