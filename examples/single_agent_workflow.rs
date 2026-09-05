//! Single-agent behavior implemented as a workflow.
//!
//! ```text
//!                  workflow: single-agent-workflow
//!                 +--------------------------------------+
//!                 |                                      |
//! user input ---->| [start]                              |
//!                 |    |                                 |
//!                 |    v                                 |
//!                 | [assistant]                          |
//!                 | SingleAgent("workspace-assistant")   |
//!                 |    |                                 |
//!                 |    v                                 |
//! final text <----| [end]                                |
//!                 |                                      |
//!                 +--------------------------------------+
//! ```

use std::io::{self, Write};

use fae_agent::{
    FAEWorkflowMetadataLoader, Session, SessionEvent, SessionEventData, SingleAgentSource,
    WorkflowAction, WorkflowEnv, WorkflowMetadata, WorkflowMetadataBuilder,
};
use fae_engine::EngineBuilder;
use serde_json::{Value, json};

const WORKFLOW_ID: &str = "single-agent-workflow";
const AGENT_ID: &str = "workspace-assistant";

fn build_workflow() -> anyhow::Result<WorkflowMetadata> {
    let mut builder = WorkflowMetadataBuilder::new(WORKFLOW_ID);
    builder.start("start", "assistant")?;
    builder.execute(
        "assistant",
        WorkflowAction::SingleAgent {
            source: SingleAgentSource::AgentId(AGENT_ID.to_string()),
            input: json!("{$input}"),
        },
        "end",
    )?;
    builder.end("end", Some(json!("{$assistant}")))?;
    builder.build()
}

async fn build_engine(loader: FAEWorkflowMetadataLoader) -> fae_engine::Engine {
    let mut builder = EngineBuilder::new();
    builder.add_runtime(fae_engine::WorkflowRuntime::with_metadata_loader(
        loader.clone(),
    ));
    builder.add_runtime(fae_engine::ModelRuntime::new());
    builder.add_runtime(fae_engine::SessionRuntime::new());
    builder.add_runtime(fae_engine::SkillRuntime::new());
    builder.add_runtime(fae_engine::McpRuntime::new());

    let mut tools = fae_engine::ToolsRuntime::new();
    tools.add_tool(Box::new(fae_engine::DefaultTools::default()));
    builder.add_runtime(tools);

    builder.add_plan_builder(fae_agent::SingleAgentPlanBuilder::new());
    builder.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(loader));
    builder.build().await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let input = read_user_input()?;
    let loader = FAEWorkflowMetadataLoader::new();
    loader.add(build_workflow()?)?;
    let engine = build_engine(loader).await;
    let (env, session) = WorkflowEnv::new(WORKFLOW_ID, Value::String(input));

    let execution = engine.launch(env).await?;
    print_session(&session).await?;
    let output = execution.result::<Value>().await?;

    println!("\nworkflow output> {}", display_value(&output));
    engine.exit().await?;
    Ok(())
}

fn read_user_input() -> anyhow::Result<String> {
    loop {
        print!("user> ");
        io::stdout().flush()?;

        let mut input = String::new();
        anyhow::ensure!(
            io::stdin().read_line(&mut input)? != 0,
            "standard input closed before a message was entered"
        );

        let input = input.trim();
        if !input.is_empty() {
            return Ok(input.to_string());
        }
    }
}

async fn print_session(session: &impl Session<(), SessionEvent>) -> anyhow::Result<()> {
    let mut streaming = None;

    while let Some(event) = session.answer().await? {
        let terminal = event.is_terminal();
        let turn_id = event.turn_id.unwrap_or_default();
        let source = event.source;

        match event.data {
            SessionEventData::TurnStarted { input } => {
                println!("\n== Turn {turn_id} | {source} ==\nuser> {input}");
            }
            SessionEventData::UserInput { content } => {
                finish_stream(&mut streaming);
                println!("user> {content}");
            }
            SessionEventData::ModelReasoning { content } => {
                begin_stream(&mut streaming, "reasoning");
                print!("{content}");
                io::stdout().flush()?;
            }
            SessionEventData::ModelOutput { content } => {
                begin_stream(&mut streaming, "assistant");
                print!("{content}");
                io::stdout().flush()?;
            }
            SessionEventData::ToolCall { arguments, .. } => {
                finish_stream(&mut streaming);
                println!("tool call> {source}\n{}", pretty_json(&arguments));
            }
            SessionEventData::ToolOutput {
                output, completed, ..
            } => {
                finish_stream(&mut streaming);
                let status = if completed { "completed" } else { "streaming" };
                println!("tool result> {source} [{status}]\n{}", pretty_json(&output));
            }
            SessionEventData::Completed { .. } => {
                finish_stream(&mut streaming);
                println!("== Turn {turn_id} completed ==");
            }
            SessionEventData::Failed { error } => {
                finish_stream(&mut streaming);
                eprintln!("workflow failed> {error}");
            }
            SessionEventData::Custom {
                event_type,
                content,
            } => {
                finish_stream(&mut streaming);
                println!("{event_type}> {content}");
            }
            SessionEventData::NodeCompleted { .. } => {}
        }

        if terminal {
            break;
        }
    }
    finish_stream(&mut streaming);
    Ok(())
}

fn begin_stream(streaming: &mut Option<&'static str>, kind: &'static str) {
    if *streaming != Some(kind) {
        finish_stream(streaming);
        print!("{kind}> ");
        *streaming = Some(kind);
    }
}

fn finish_stream(streaming: &mut Option<&'static str>) {
    if streaming.take().is_some() {
        println!();
    }
}

fn pretty_json(value: &str) -> String {
    serde_json::from_str::<Value>(value)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .unwrap_or_else(|_| value.to_string())
}

fn display_value(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_wraps_single_agent_output() -> anyhow::Result<()> {
        let workflow = build_workflow()?;

        assert!(matches!(
            &workflow.nodes["assistant"],
            fae_agent::WorkflowNode::Execute {
                action: WorkflowAction::SingleAgent {
                    source: SingleAgentSource::AgentId(agent_id),
                    input,
                },
                next,
            } if agent_id == AGENT_ID
                && input == &json!("{$input}")
                && next == &["end"]
        ));
        assert!(matches!(
            &workflow.nodes["end"],
            fae_agent::WorkflowNode::End {
                output: Some(output)
            } if output == &json!("{$assistant}")
        ));
        Ok(())
    }
}
