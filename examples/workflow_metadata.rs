use fae_agent::{
    SingleAgentSource, WorkflowAction, WorkflowCompare, WorkflowCondition, WorkflowMetadata,
    WorkflowMetadataBuilder,
};
use fae_engine::{READ_FILE, WRITE_FILE};
use serde_json::{Value, json};

pub const PYTHON_ACTION_TASK_TYPE: &str = "workflow.python";

/// 构建完整的发布就绪检查流程：
///
/// 1. 根据输入选择严格或快速检查策略。
/// 2. 并行读取并审查 Rust 源码和 Cargo 清单。
/// 3. 使用 Python 汇总两个 Agent 的审查结果。
/// 4. 根据汇总结果决定是否执行限定次数的整改子流程。
/// 5. 由 release-manager Agent 生成最终报告。
///
/// ```text
///                               +-----------------+
///                               |      start      |
///                               +--------+--------+
///                                        |
///                               +--------v--------+
///                               |  select_policy  |
///                               +---+---------+---+
///                         strict   |         |   quick
///                    +-------------+         +-------------+
///                    |                                       |
///           +--------v--------+                     +--------v-------+
///           |  strict_policy  |                     |  quick_policy  |
///           +--------+--------+                     +--------+-------+
///                    +-------------------+-------------------+
///                                        |
///                              +---------v---------+
///                              |  dispatch_checks  |
///                              +----+----------+---+
///                                   |          |
///                       +-----------+          +-----------+
///                       |                                  |
///              +--------v--------+                +--------v---------+
///              |   read_source   |                |  read_manifest   |
///              +--------+--------+                +--------+---------+
///                       |                                  |
///              +--------v--------+                +--------v---------+
///              | review_source   |                | review_manifest  |
///              |    (Agent)      |                |     (Agent)      |
///              +--------+--------+                +--------+---------+
///                       +---------------+------------------+
///                                       |
///                            +----------v-----------+
///                            |  aggregate_reviews   |
///                            |      (Python)        |
///                            +----------+-----------+
///                                       |
///                            +----------v-----------+
///                            |  route_remediation   |
///                            +---+--------------+---+
///                         yes    |              |    no
///                   +------------+              +------------+
///                   |                                        |
///          +--------v---------+                    +---------v----------+
///          |   remediation    |                    | skip_remediation   |
///          | (nested workflow)|                    |     (Python)       |
///          +--------+---------+                    +---------+----------+
///                   +-------------------+--------------------+
///                                       |
///                              +--------v--------+
///                              |  final_report   |
///                              |     (Agent)     |
///                              +--------+--------+
///                                       |
///                                  +----v----+
///                                  |   end   |
///                                  +---------+
/// ```
pub fn build_release_review_workflow() -> anyhow::Result<WorkflowMetadata> {
    let mut builder = WorkflowMetadataBuilder::new("release-readiness-review");

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
            json!("策略：{$input.policy}\n路径：{$read_source.path}\n\n{$read_source.content}"),
        ),
        "aggregate_reviews",
    )?;
    builder.execute(
        "review_manifest",
        agent_action(
            "manifest-reviewer",
            json!("策略：{$input.policy}\n路径：{$read_manifest.path}\n\n{$read_manifest.content}"),
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
            workflow_id: "bounded-remediation-loop".to_string(),
            input: json!({
                "counter_path": "{$input.counter_path}",
                "rounds": "{$input.remediation_rounds}"
            }),
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
            json!({
                "policy": "{$input.policy}",
                "quality_gate": "{$aggregate_reviews}",
                "remediation_requested": "{$input.run_remediation}"
            }),
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

/// 构建用于演示有限循环的顺序子流程。
///
/// 首先通过写文件验证普通工具调用，随后将循环状态保存在每次 Python
/// 动作的输出中，并通过 `{$last.remaining}` 读取：
/// `rounds -> rounds - 1 -> ... -> 0`.
///
/// ```text
/// +-------+     +--------------+     +------------------+
/// | start | --> | seed_counter | --> | initialize_state |
/// +-------+     | (write_file) |     |     (Python)     |
///               +--------------+     +--------+---------+
///                                             |
///                                    +--------v--------+
///                              +---->|   retry_loop    |---- remaining == 0 ----+
///                              |     +--------+--------+                        |
///                              |              | remaining > 0                  |
///                              |     +--------v------------+                   |
///                              +-----+ decrement_counter   |                   |
///                                    |      (Python)       |                   |
///                                    +---------------------+                   |
///                                                                              |
///                                                                    +---------v--+
///                                                                    |    done    |
///                                                                    +------------+
/// ```
pub fn build_remediation_workflow() -> anyhow::Result<WorkflowMetadata> {
    let mut builder = WorkflowMetadataBuilder::new("bounded-remediation-loop");
    builder.start("start", "seed_counter")?;
    // 进入循环前，将请求执行的轮数记录到文件中。
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
    // 将 workflow 输入转换为初始循环状态。
    builder.execute(
        "initialize_state",
        python_action(
            r#"result = {"remaining": arguments["rounds"]}"#,
            json!({"rounds": "{$input.rounds}"}),
        ),
        "retry_loop",
    )?;
    // 当最近一次 Python 输出仍有剩余轮数时继续循环。
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
    // 每执行一次循环体，代表完成一轮整改。
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

fn agent_action(name: &str, input: Value) -> WorkflowAction {
    WorkflowAction::SingleAgent {
        source: SingleAgentSource::AgentId(name.to_string()),
        input,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::WorkflowNode;

    #[test]
    fn metadata_covers_parallel_choice_loop_tools_agents_and_python() {
        let workflow = build_release_review_workflow().unwrap();

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
            action: WorkflowAction::Workflow {
                workflow_id, input, ..
            },
            ..
        } = &workflow.nodes["remediation"]
        else {
            panic!("remediation must execute a nested workflow");
        };
        assert_eq!(workflow_id, "bounded-remediation-loop");
        assert_eq!(input["rounds"], "{$input.remediation_rounds}");
        assert!(WorkflowMetadata::from_json(&workflow.to_json().unwrap()).is_ok());
    }
}
