use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use fae_agent::{
    Event, EventType, FAEWorkflowMetadataLoader, RuntimeSelectExec, Session, SingleAgentEnv,
    SingleAgentInfo, SingleAgentModelConfig, SkillQuery, TaskError, TaskReq, TaskResp, TaskType,
    WorkflowActionRequest, WorkflowActionResponse, WorkflowEnv,
};
use fae_engine::{
    DefaultTools, Engine, EngineBuilder, McpRuntime, ModelRuntime, PlanRuntime, SessionRuntime,
    SkillRuntime, ToolsRuntime, WorkflowRuntime,
};
use serde_json::Value;
use wd_tools::channel::{Channel, Receiver, Sender};

use crate::{
    args::{AgentArgs, Cli, Command, WorkflowArgs},
    tui::{Mode, PromptAction, TerminalUi},
};

const PYTHON_ACTION_TASK_TYPE: &str = "workflow.python";

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Some(Command::Agent(args)) => run_agent(args, cli.color, cli.no_alt_screen).await,
        Some(Command::Workflow(args)) => {
            run_workflow(args, cli.fae_home, cli.color, cli.no_alt_screen).await
        }
        None => unreachable!("default agent command is inserted before parsing"),
    }
}

async fn run_agent(
    args: AgentArgs,
    color: crate::args::ColorChoice,
    no_alt_screen: bool,
) -> anyhow::Result<()> {
    let model = args
        .model
        .clone()
        .filter(|model| !model.trim().is_empty())
        .context("set FAE_DEFAULT_MODEL or pass --model")?;
    let session_id = args.session.clone().unwrap_or_else(default_session_id);
    let loader = FAEWorkflowMetadataLoader::new();
    let engine = build_engine(loader).await;

    let mut ui = TerminalUi::new(Mode::Agent, &model, &session_id, color, no_alt_screen)?;
    let first_input = if args.prompt.is_empty() {
        next_agent_input(&mut ui, &model, &session_id).await?
    } else {
        let input = args.prompt.join(" ");
        ui.push_user(&input);
        Some(input)
    };

    let result = async {
        let Some(input) = first_input else {
            return Ok(());
        };
        let skills = args.skills.iter().map(|skill| skill_query(skill)).collect();
        let (env, session) = SingleAgentEnv::new(
            SingleAgentInfo {
                name: "fae".to_string(),
                user_id: args.user.clone(),
                session_id: session_id.clone(),
                metadata: HashMap::new(),
            },
            args.system_prompt.clone(),
            SingleAgentModelConfig {
                model: model.clone(),
                context_size: args.context_size,
                history_turns: args.history_turns,
                max_completion_tokens: Some(args.max_completion_tokens),
                temperature: None,
                max_tool_iterations: args.max_tool_iterations,
            },
            input,
            args.tools.clone(),
        );
        let env = env
            .with_skills(skills)
            .with_mcp_servers(args.mcp_servers.clone());
        let execution = engine.launch(env).await?;

        if !ui.run_session(&session, Some(&execution)).await? {
            return Ok(());
        }
        execution.result::<()>().await?;

        loop {
            let Some(input) = next_agent_input(&mut ui, &model, &session_id).await? else {
                break;
            };
            session.call(input).await?;
            if !ui.run_session(&session, None).await? {
                break;
            }
        }
        Ok(())
    }
    .await;

    drop(ui);
    engine.exit().await?;
    result
}

async fn run_workflow(
    args: WorkflowArgs,
    fae_home: Option<PathBuf>,
    color: crate::args::ColorChoice,
    no_alt_screen: bool,
) -> anyhow::Result<()> {
    let loader = match fae_home {
        Some(home) => FAEWorkflowMetadataLoader::with_home_dir(expand_home(home)),
        None => FAEWorkflowMetadataLoader::new(),
    };
    let input = parse_workflow_input(&args.input).await?;
    let engine = build_engine(loader.clone()).await;
    let model = std::env::var("FAE_DEFAULT_MODEL").unwrap_or_else(|_| "workflow".to_string());
    let mut ui = TerminalUi::new(Mode::Workflow, model, &args.id, color, no_alt_screen)?;
    ui.push_system(format!(
        "Loading {} from {}",
        args.id,
        loader.home_dir().join("workflows").display()
    ));
    let (env, session) = WorkflowEnv::new(&args.id, input);

    let result = async {
        let execution = engine.launch(env).await?;
        if !ui.run_session(&session, Some(&execution)).await? {
            return Ok(());
        }
        let output = execution.result::<Value>().await?;
        ui.workflow_result(&output);
        ui.wait_for_close().await?;
        Ok(())
    }
    .await;

    drop(ui);
    engine.exit().await?;
    result
}

async fn next_agent_input(
    ui: &mut TerminalUi,
    model: &str,
    session_id: &str,
) -> anyhow::Result<Option<String>> {
    loop {
        let PromptAction::Submit(input) = ui.prompt().await? else {
            return Ok(None);
        };
        match input.as_str() {
            "/exit" | "/quit" => return Ok(None),
            "/help" => {
                ui.push_system(
                    "/help  show commands\n/status  show model and session\n/clear  clear the transcript\n/exit  leave the session",
                );
            }
            "/status" => {
                ui.push_system(format!("model: {model}\nsession: {session_id}"));
            }
            "/clear" => ui.clear_transcript(),
            command if command.starts_with('/') => {
                ui.push_system(format!("Unknown command `{command}`. Use /help."));
            }
            _ => {
                ui.push_user(&input);
                return Ok(Some(input));
            }
        }
    }
}

fn skill_query(value: &str) -> SkillQuery {
    let path = Path::new(value);
    if path.exists() || value.ends_with("SKILL.md") || value.contains(std::path::MAIN_SEPARATOR) {
        SkillQuery::Path(expand_home(path.to_path_buf()))
    } else {
        SkillQuery::Name(value.to_string())
    }
}

async fn parse_workflow_input(input: &str) -> anyhow::Result<Value> {
    let (source, label) = if let Some(path) = input.strip_prefix('@') {
        anyhow::ensure!(!path.is_empty(), "workflow input path cannot be empty");
        let path = expand_home(PathBuf::from(path));
        (
            tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("read workflow input `{}`", path.display()))?,
            path.display().to_string(),
        )
    } else {
        (input.to_string(), "--input".to_string())
    };

    serde_json::from_str(&source).with_context(|| format!("parse workflow JSON from {label}"))
}

fn expand_home(path: PathBuf) -> PathBuf {
    let Some(path_text) = path.to_str() else {
        return path;
    };
    let Some(home) = std::env::var_os("HOME") else {
        return path;
    };
    if path_text == "~" {
        return PathBuf::from(home);
    }
    path_text
        .strip_prefix("~/")
        .map(|rest| PathBuf::from(home).join(rest))
        .unwrap_or(path)
}

fn default_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("terminal-{timestamp}-{}", std::process::id())
}

async fn build_engine(loader: FAEWorkflowMetadataLoader) -> Engine {
    let mut builder = EngineBuilder::new();
    builder.add_runtime(PlanRuntime::new());
    builder.add_runtime(WorkflowRuntime::with_metadata_loader(loader.clone()));
    builder.add_runtime(ModelRuntime::new());
    builder.add_runtime(SessionRuntime::new());
    builder.add_runtime(SkillRuntime::new());
    builder.add_runtime(McpRuntime::new());
    builder.add_runtime(PythonActionRuntime::default());

    let mut tools = ToolsRuntime::new();
    tools.add_tool(Box::new(DefaultTools::default()));
    builder.add_runtime(tools);

    builder.add_plan_builder(fae_agent::SingleAgentPlanBuilder);
    builder.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(loader));
    builder.build().await
}

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

    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn parses_inline_and_file_workflow_input() {
        assert_eq!(
            parse_workflow_input(r#"{"enabled":true}"#).await.unwrap(),
            json!({"enabled": true})
        );

        let path = std::env::temp_dir().join(format!("fae-input-{}.json", std::process::id()));
        tokio::fs::write(&path, r#"{"count":3}"#).await.unwrap();
        let input = parse_workflow_input(&format!("@{}", path.display()))
            .await
            .unwrap();
        tokio::fs::remove_file(path).await.unwrap();
        assert_eq!(input, json!({"count": 3}));
    }

    #[test]
    fn treats_names_and_paths_as_distinct_skill_queries() {
        assert_eq!(
            skill_query("weather"),
            SkillQuery::Name("weather".to_string())
        );
        assert!(matches!(
            skill_query("./skills/review/SKILL.md"),
            SkillQuery::Path(_)
        ));
    }

    #[tokio::test]
    async fn python_runtime_returns_json_value() {
        let output = execute_python(
            "result = arguments['left'] + arguments['right']",
            &json!({"left": 2, "right": 3}),
        )
        .await
        .unwrap();
        assert_eq!(output, json!(5));
    }

    #[tokio::test]
    async fn app_engine_runs_registered_workflow() {
        let mut workflow = fae_agent::WorkflowMetadataBuilder::new("terminal-smoke-test");
        workflow.start("start", "end").unwrap();
        workflow
            .end("end", Some(json!({"value": "{$input.value}"})))
            .unwrap();

        let loader = FAEWorkflowMetadataLoader::new();
        loader.add(workflow.build().unwrap()).unwrap();
        let engine = build_engine(loader).await;
        let (env, _) = WorkflowEnv::new("terminal-smoke-test", json!({"value": 42}));
        let (_, output) = engine.invoke::<_, Value>(env).await.unwrap();
        engine.exit().await.unwrap();

        assert_eq!(output, json!({"value": 42}));
    }
}
