mod workflow_metadata;

use std::{
    io::{self, Write},
    path::Path,
};

use fae_agent::{
    Event, EventType, FAEWorkflowMetadataLoader, RuntimeSelectExec, Session, SessionEvent,
    SessionEventData, SingleAgentModelConfig, TaskError, TaskReq, TaskResp, TaskType,
    WorkflowActionRequest, WorkflowActionResponse, WorkflowEnv,
};
use fae_engine::EngineBuilder;
use serde_json::{Value, json};
use wd_tools::channel::{Channel, Receiver, Sender};

use workflow_metadata::{PYTHON_ACTION_TASK_TYPE, build_release_review_workflow};

#[derive(Debug)]
struct PythonActionRuntime {
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Default for PythonActionRuntime {
    fn default() -> Self {
        let (event_sender, event_receiver) = Channel::new(128);
        Self {
            event_sender,
            event_receiver,
        }
    }
}

impl PythonActionRuntime {
    async fn execute(
        task: TaskReq<WorkflowActionRequest>,
    ) -> fae_agent::Result<TaskResp<WorkflowActionResponse>> {
        if task.req.action != "python" {
            return Err(
                anyhow::anyhow!("unsupported workflow action `{}`", task.req.action).into(),
            );
        }

        let code = task.req.payload["code"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("python action is missing string field `code`"))?;
        let arguments = task
            .req
            .payload
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Null);
        let output = execute_python(code, &arguments).await?;

        Ok(TaskResp {
            ctx: task.ctx,
            meta: task.meta,
            resp: WorkflowActionResponse { output },
        })
    }
}

#[async_trait::async_trait]
impl RuntimeSelectExec<WorkflowActionRequest, WorkflowActionResponse, (), ()>
    for PythonActionRuntime
{
    fn id(&self) -> &str {
        PYTHON_ACTION_TASK_TYPE
    }

    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Any(PYTHON_ACTION_TASK_TYPE.to_string())]
    }

    async fn watch(&self) -> fae_agent::Result<Receiver<fae_agent::Event>> {
        Ok(self.event_receiver.clone())
    }

    async fn spawn(&self, task: TaskReq<WorkflowActionRequest>) -> fae_agent::Result<()> {
        let event_sender = self.event_sender.clone();
        tokio::spawn(async move {
            let ctx = task.ctx.clone();
            let meta = task.meta.clone();
            let event_type = match Self::execute(task).await {
                Ok(response) => EventType::TaskResult(response.into_response()),
                Err(error) => EventType::TaskError(TaskError {
                    ctx,
                    meta,
                    error: error.to_string(),
                }),
            };
            let _ = event_sender
                .send(Event {
                    from_rt_id: PYTHON_ACTION_TASK_TYPE.to_string(),
                    event_type,
                })
                .await;
        });
        Ok(())
    }

    async fn exec(
        &self,
        task: TaskReq<WorkflowActionRequest>,
    ) -> fae_agent::Result<TaskResp<WorkflowActionResponse>> {
        Self::execute(task).await
    }
}

async fn execute_python(code: &str, arguments: &Value) -> anyhow::Result<Value> {
    let program = format!(
        concat!(
            "import json\n",
            "arguments = json.loads({arguments_json})\n",
            "result = None\n",
            "{code}\n",
            "print(json.dumps(result, ensure_ascii=False))\n"
        ),
        arguments_json = serde_json::to_string(&serde_json::to_string(arguments)?)?,
        code = code,
    );
    let output = tokio::process::Command::new("python3")
        .arg("-c")
        .arg(program)
        .kill_on_drop(true)
        .output()
        .await?;

    anyhow::ensure!(
        output.status.success(),
        "python action failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn build_engine(loader: FAEWorkflowMetadataLoader) -> fae_engine::Engine {
    let mut builder = EngineBuilder::new();
    builder.add_runtime(fae_engine::PlanRuntime::new());
    builder.add_runtime(fae_engine::WorkflowRuntime::with_metadata_loader(
        loader.clone(),
    ));
    builder.add_runtime(fae_engine::ModelRuntime::new());
    builder.add_runtime(fae_engine::SessionRuntime::new());
    builder.add_runtime(PythonActionRuntime::default());

    let mut tools = fae_engine::ToolsRuntime::new();
    tools.add_tool(Box::new(fae_engine::DefaultTools::default()));
    builder.add_runtime(tools);

    builder.add_plan_builder(fae_agent::SingleAgentPlanBuilder);
    builder.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(loader));
    builder.build().await
}

async fn print_session(session: &impl Session<(), SessionEvent>) -> anyhow::Result<()> {
    let mut streaming = None;
    println!("\n=== WORKFLOW SESSION (LIVE) ===");

    while let Some(event) = session.answer().await? {
        let terminal = event.is_terminal();
        print_session_event(&event, &mut streaming)?;
        if terminal {
            break;
        }
    }
    finish_stream(&mut streaming);
    println!("\n=== WORKFLOW SESSION COMPLETE ===");
    Ok(())
}

fn print_session_event(
    event: &SessionEvent,
    streaming: &mut Option<(String, &'static str)>,
) -> anyhow::Result<()> {
    let stream_kind = match &event.data {
        SessionEventData::ModelReasoning { .. } => Some("reasoning"),
        SessionEventData::ModelOutput { .. } => Some("assistant"),
        _ => None,
    };

    if let Some(kind) = stream_kind {
        let stream_id = format!(
            "{} / {} / turn {}",
            event.node_id.as_deref().unwrap_or("-"),
            event.source,
            event.turn_id.unwrap_or_default()
        );
        if streaming.as_ref() != Some(&(stream_id.clone(), kind)) {
            finish_stream(streaming);
            println!("\n--- single agent: {stream_id} ---");
            print!("{kind}> ");
            *streaming = Some((stream_id, kind));
        }

        let content = match &event.data {
            SessionEventData::ModelReasoning { content }
            | SessionEventData::ModelOutput { content } => content,
            _ => unreachable!(),
        };
        print!("{content}");
        io::stdout().flush()?;
        return Ok(());
    }

    finish_stream(streaming);
    println!(
        "\n--- session event: {} | node {} ---\n{}",
        event.kind(),
        event.node_id.as_deref().unwrap_or("-"),
        serde_json::to_string_pretty(event)?
    );
    Ok(())
}

fn finish_stream(streaming: &mut Option<(String, &'static str)>) {
    if streaming.take().is_some() {
        println!();
    }
}

fn workflow_input(example_dir: &Path) -> Value {
    json!({
        "policy": "strict",
        "run_remediation": true,
        "source_path": example_dir.join("workflow.rs"),
        "manifest_path": example_dir.join("Cargo.toml"),
        "counter_path": example_dir.join("target/workflow-remediation.txt"),
        "remediation_rounds": 2
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = std::env::var("FAE_DEFAULT_MODEL")
        .map_err(|_| anyhow::anyhow!("set FAE_DEFAULT_MODEL before running this example"))?;
    let model_config = SingleAgentModelConfig {
        model,
        context_size: 32_000,
        history_turns: 1,
        max_completion_tokens: Some(1_024),
        temperature: Some(0.0),
        max_tool_iterations: 1,
    };
    let input = workflow_input(Path::new(env!("CARGO_MANIFEST_DIR")));
    let loader = FAEWorkflowMetadataLoader::new();
    let engine = build_engine(loader.clone()).await;
    loader.add(build_release_review_workflow(model_config)?)?;
    loader.add(workflow_metadata::build_remediation_workflow()?)?;
    let (env, session) = WorkflowEnv::new("release-readiness-review", input);

    let execution = engine.launch(env).await?;
    // 在 workflow 执行期间持续消费 session，实时输出节点和 Agent 事件。
    print_session(&session).await?;
    let output = execution.result::<Value>().await?;
    println!(
        "\n=== WORKFLOW OUTPUT ===\n{}",
        serde_json::to_string_pretty(&output)?
    );

    engine.exit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use workflow_metadata::build_remediation_workflow;

    #[tokio::test]
    async fn executes_python_tool_and_loop_actions() -> anyhow::Result<()> {
        // 此测试只执行有限整改循环子流程；完整的发布检查 DAG 由 main 演示。
        let counter_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("workflow-remediation-{}.txt", std::process::id()));
        let input = json!({
            "counter_path": &counter_path,
            "rounds": 2
        });
        let loader = FAEWorkflowMetadataLoader::new();
        let engine = build_engine(loader.clone()).await;
        loader.add(build_remediation_workflow()?)?;
        let (env, session) = WorkflowEnv::new("bounded-remediation-loop", input);

        // invoke() 会等待流程结束，因此需要并发消费 session，确保使用
        // `--nocapture` 运行测试时能够实时看到事件。
        let (execution, ()) =
            tokio::try_join!(engine.invoke::<_, Value>(env), print_session(&session))?;
        let (_, output) = execution;

        assert_eq!(output["iterations"], 2);
        assert_eq!(output["remaining"], 0);
        engine.exit().await?;
        tokio::fs::remove_file(counter_path).await?;
        Ok(())
    }
}
