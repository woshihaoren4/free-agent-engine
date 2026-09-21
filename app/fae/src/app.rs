use std::{
    io::{self, Write},
    path::PathBuf,
};

use anyhow::Context;
use fae_agent::{
    Event, EventType, FAEWorkflowMetadataLoader, RuntimeSelectExec, Session, SessionEventData,
    SingleAgentEnv, SingleAgentPlanBuilder, SingleAgentSource, TaskError, TaskReq, TaskResp,
    TaskType, WorkflowActionRequest, WorkflowActionResponse, WorkflowEnv,
};
use fae_engine::{
    CompressionRuntime, DefaultTools, Engine, EngineBuilder, McpRuntime, ModelRuntime, PlanRuntime,
    SessionRuntime, SkillRuntime, ToolsRuntime, UserMemoryRuntime, WorkflowRuntime,
    default_fae_host,
};
use serde_json::Value;
use wd_tools::channel::{Channel, Receiver, Sender};

use crate::{
    args::{AgentArgs, Cli, Command, WorkflowArgs},
    init::initialize,
    tui::{Mode, PromptAction, TerminalUi},
    workspace::{WorkspaceHookBuilder, resolve_workspace},
};

const PYTHON_ACTION_TASK_TYPE: &str = "workflow.python";
const SESSION_HELP: &str = "\
Usage: /session <command>

Commands:
  new      start a new session
  id=<id>  switch sessions
  clean    clear current session history";

#[derive(Debug, PartialEq, Eq)]
enum AgentPromptAction {
    Submit(String),
    RestartSession,
    Exit,
}

#[derive(Debug, PartialEq, Eq)]
enum SessionCommand {
    Help,
    New,
    Switch(String),
    Clean,
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Some(Command::Init(args)) => {
            let home = cli
                .fae_home
                .map(expand_home)
                .unwrap_or_else(default_fae_host);
            let result = initialize(&home, &args).await?;
            println!("Initialized agent `{}`.", args.agent_id);
            println!("Config: {}", result.config_path.display());
            println!(
                "Enabled {} tools and {} installed skills.",
                fae_engine::DEFAULT_TOOL_NAMES.len(),
                result.skill_count
            );
            if result.skill_count == 0 {
                println!(
                    "No installed skills found in {}.",
                    home.join("skills").display()
                );
            }
            Ok(())
        }
        Some(Command::Uninstall) => {
            let executable = std::env::current_exe().context("locate current fae executable")?;
            uninstall(&executable).await?;
            println!("Removed {}.", executable.display());
            Ok(())
        }
        Some(Command::Agent(args)) => {
            run_agent(
                args,
                cli.fae_home,
                cli.workspace,
                cli.color,
                cli.no_alt_screen,
            )
            .await
        }
        Some(Command::Workflow(args)) => {
            run_workflow(
                args,
                cli.fae_home,
                cli.workspace,
                cli.color,
                cli.no_alt_screen,
            )
            .await
        }
        None => unreachable!("default agent command is inserted before parsing"),
    }
}

async fn uninstall(executable: &std::path::Path) -> anyhow::Result<()> {
    tokio::fs::remove_file(executable)
        .await
        .with_context(|| format!("remove fae executable `{}`", executable.display()))
}

async fn run_agent(
    args: AgentArgs,
    fae_home: Option<PathBuf>,
    workspace: PathBuf,
    color: crate::args::ColorChoice,
    no_alt_screen: bool,
) -> anyhow::Result<()> {
    let loader = match fae_home {
        Some(home) => FAEWorkflowMetadataLoader::with_home_dir(expand_home(home)),
        None => FAEWorkflowMetadataLoader::new(),
    };
    let source = match (args.agent_config.clone(), args.agent_prompt.clone()) {
        (Some(config), Some(prompt)) => SingleAgentSource::Paths {
            config: expand_home(config),
            prompt: expand_home(prompt),
        },
        (None, None) => SingleAgentSource::AgentId(args.agent_id.clone()),
        _ => unreachable!("clap requires agent config and prompt paths together"),
    };
    let workspace = resolve_workspace(expand_home(workspace))?;
    let mut agent_builder = SingleAgentPlanBuilder::with_home_dir(loader.home_dir());
    agent_builder.add_hook(WorkspaceHookBuilder::new(workspace));
    let (config, _) = agent_builder.load_config(&source).await?;
    let mut session_id = args.session_id.unwrap_or(config.agent.session_id);
    anyhow::ensure!(!session_id.trim().is_empty(), "session_id cannot be empty");
    let agent_name = config.agent.name;
    let model = config.model.model;
    let user_id = config.agent.user_id;
    let session_runtime = SessionRuntime::with_host_dir(loader.home_dir());
    let engine = build_engine(loader, agent_builder).await;

    if !args.prompt.is_empty() {
        let input = args.prompt.join(" ");
        let (env, session) = SingleAgentEnv::new_with_user_id(source, input, user_id);
        let execution = engine.launch(env.with_session_id(session_id)).await?;
        let result = stream_agent_output(&session).await;
        let execution_result = execution.result::<()>().await;
        engine.exit().await?;
        result?;
        return execution_result;
    }

    let mut ui = TerminalUi::new(
        Mode::Agent,
        &agent_name,
        Some(user_id.clone()),
        &model,
        &session_id,
        color,
        no_alt_screen,
    )?;

    let result = async {
        'sessions: loop {
            let input = loop {
                match next_agent_input(
                    &mut ui,
                    &model,
                    &mut session_id,
                    &agent_name,
                    &user_id,
                    &session_runtime,
                )
                .await?
                {
                    AgentPromptAction::Submit(input) => break input,
                    AgentPromptAction::RestartSession => continue,
                    AgentPromptAction::Exit => return Ok(()),
                }
            };

            let (env, session) =
                SingleAgentEnv::new_with_user_id(source.clone(), input, user_id.clone());
            let execution = match engine.launch(env.with_session_id(session_id.clone())).await {
                Ok(execution) => execution,
                Err(error) => {
                    ui.push_error(format!("{error:#}"));
                    continue;
                }
            };

            let completed = match ui
                .run_session(
                    &session,
                    Some(&execution),
                    Some(|content| fae_agent::SessionInput::Supplement(content.into())),
                )
                .await
            {
                Ok(completed) => completed,
                Err(error) => {
                    ui.push_error(format!("{error:#}"));
                    continue;
                }
            };
            if !completed {
                return Ok(());
            }
            if let Err(error) = execution.result::<()>().await {
                ui.push_error(format!("{error:#}"));
                continue;
            }

            loop {
                let input = match next_agent_input(
                    &mut ui,
                    &model,
                    &mut session_id,
                    &agent_name,
                    &user_id,
                    &session_runtime,
                )
                .await?
                {
                    AgentPromptAction::Submit(input) => input,
                    AgentPromptAction::RestartSession => continue 'sessions,
                    AgentPromptAction::Exit => return Ok(()),
                };
                if let Err(error) = session
                    .call(fae_agent::SessionInput::NewChat(input.into()))
                    .await
                {
                    ui.push_error(format!("{error:#}"));
                    continue;
                }
                let completed = match ui
                    .run_session(
                        &session,
                        None,
                        Some(|content| fae_agent::SessionInput::Supplement(content.into())),
                    )
                    .await
                {
                    Ok(completed) => completed,
                    Err(error) => {
                        ui.push_error(format!("{error:#}"));
                        continue 'sessions;
                    }
                };
                if !completed {
                    return Ok(());
                }
            }
        }
    }
    .await;

    drop(ui);
    engine.exit().await?;
    result
}

async fn stream_agent_output(
    session: &impl Session<fae_agent::SessionInput, fae_agent::SessionOutput>,
) -> anyhow::Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut wrote_output = false;

    while let Some(event) = session.answer().await? {
        let terminal = event.is_terminal();
        if event.parament_plan_id.is_none()
            && let SessionEventData::ModelOutput { content } = event.event_data()?
        {
            stdout.write_all(content.as_bytes())?;
            stdout.flush()?;
            wrote_output = true;
        }
        if terminal {
            break;
        }
    }

    if wrote_output {
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

async fn run_workflow(
    args: WorkflowArgs,
    fae_home: Option<PathBuf>,
    workspace: PathBuf,
    color: crate::args::ColorChoice,
    no_alt_screen: bool,
) -> anyhow::Result<()> {
    let loader = match fae_home {
        Some(home) => FAEWorkflowMetadataLoader::with_home_dir(expand_home(home)),
        None => FAEWorkflowMetadataLoader::new(),
    };
    let input = parse_workflow_input(&args.input).await?;
    let workspace = resolve_workspace(expand_home(workspace))?;
    let mut agent_builder = SingleAgentPlanBuilder::with_home_dir(loader.home_dir());
    agent_builder.add_hook(WorkspaceHookBuilder::new(workspace));
    let engine = build_engine(loader.clone(), agent_builder).await;
    let (env, session) = WorkflowEnv::new(&args.id, input);

    if !args.interactive {
        let result = async {
            let execution = engine.launch(env).await?;
            let output = execution.result::<Value>().await?;
            println!("{}", serde_json::to_string_pretty(&output)?);
            Ok(())
        }
        .await;
        engine.exit().await?;
        return result;
    }

    let model = std::env::var("FAE_DEFAULT_MODEL").unwrap_or_else(|_| "workflow".to_string());
    let mut ui = TerminalUi::new(
        Mode::Workflow,
        &args.id,
        None,
        model,
        &args.id,
        color,
        no_alt_screen,
    )?;
    ui.push_system(format!(
        "Loading {} from {}",
        args.id,
        loader.home_dir().join("workflows").display()
    ));
    let result = async {
        let execution = engine.launch(env).await?;
        if !ui.run_session(&session, Some(&execution), None).await? {
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
    session_id: &mut String,
    agent_id: &str,
    user_id: &str,
    session_runtime: &SessionRuntime,
) -> anyhow::Result<AgentPromptAction> {
    loop {
        let PromptAction::Submit(input) = ui.prompt().await? else {
            return Ok(AgentPromptAction::Exit);
        };
        match input.as_str() {
            "/exit" | "/quit" => return Ok(AgentPromptAction::Exit),
            "/help" => {
                ui.push_system(
                    "/help  show commands\n/status  show model and session\n/session new  start a new session\n/session id=<id>  switch sessions\n/session clean, /clean  clear current session history\n/clear  clear the transcript\n/steer <message>  supplement a running agent\n/exit  leave the session",
                );
            }
            "/status" => {
                ui.push_system(format!("model: {model}\nsession: {session_id}"));
            }
            "/clear" => ui.clear_transcript(),
            command => {
                let Some(command) = parse_session_command(command) else {
                    if command.starts_with('/') {
                        ui.push_system(format!("Unknown command `{command}`. Use /help."));
                        continue;
                    }
                    ui.push_user(&input);
                    return Ok(AgentPromptAction::Submit(input));
                };
                match command {
                    Ok(SessionCommand::Help) => ui.push_system(SESSION_HELP),
                    Ok(SessionCommand::New) => {
                        *session_id = wd_tools::uuid::v4();
                        reset_session_ui(ui, session_id, "New session");
                        return Ok(AgentPromptAction::RestartSession);
                    }
                    Ok(SessionCommand::Switch(id)) => {
                        if let Err(error) = session_runtime.session_path(agent_id, user_id, &id) {
                            ui.push_system(format!("Invalid session ID\n{error}"));
                            continue;
                        }
                        *session_id = id;
                        reset_session_ui(ui, session_id, "Switched session");
                        return Ok(AgentPromptAction::RestartSession);
                    }
                    Ok(SessionCommand::Clean) => {
                        session_runtime
                            .delete(agent_id, user_id, session_id)
                            .await?;
                        reset_session_ui(ui, session_id, "Cleaned session history");
                        return Ok(AgentPromptAction::RestartSession);
                    }
                    Err(message) => ui.push_system(message),
                }
            }
        }
    }
}

fn parse_session_command(input: &str) -> Option<Result<SessionCommand, String>> {
    if input == "/clean" {
        return Some(Ok(SessionCommand::Clean));
    }
    let arguments = input.strip_prefix("/session")?;
    if !arguments.is_empty() && !arguments.starts_with(char::is_whitespace) {
        return None;
    }

    let arguments = arguments.trim();
    let command = match arguments {
        "-h" | "--help" => SessionCommand::Help,
        "new" => SessionCommand::New,
        "clean" => SessionCommand::Clean,
        value
            if value.starts_with("id=")
                && !value[3..].is_empty()
                && !value[3..].contains(char::is_whitespace) =>
        {
            SessionCommand::Switch(value[3..].to_string())
        }
        _ => {
            return Some(Err(
                "Invalid session command\nUse `/session new`, `/session id=<id>`, or `/session clean`."
                    .to_string(),
            ));
        }
    };
    Some(Ok(command))
}

fn reset_session_ui(ui: &mut TerminalUi, session_id: &str, message: &str) {
    ui.clear_transcript();
    ui.set_subject(session_id);
    ui.push_notice(format!("{message}\nsession: {session_id}"));
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

async fn build_engine(
    loader: FAEWorkflowMetadataLoader,
    agent_builder: SingleAgentPlanBuilder,
) -> Engine {
    let home_dir = loader.home_dir().to_path_buf();
    let mut builder = EngineBuilder::new();
    builder.add_runtime(PlanRuntime::new());
    builder.add_runtime(WorkflowRuntime::with_metadata_loader(loader.clone()));
    builder.add_runtime(ModelRuntime::new());
    builder.add_runtime(CompressionRuntime::default());
    builder.add_runtime(SessionRuntime::with_host_dir(&home_dir));
    builder.add_runtime(UserMemoryRuntime::with_host_dir(&home_dir));
    builder.add_runtime(SkillRuntime::with_host_dir(&home_dir));
    builder.add_runtime(McpRuntime::with_mcp_dir(home_dir.join("mcp")));
    builder.add_runtime(PythonActionRuntime::default());

    let mut tools = ToolsRuntime::new();
    tools.add_tool(Box::new(DefaultTools::default()));
    builder.add_runtime(tools);

    builder.add_plan_builder(agent_builder);
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

    #[test]
    fn parses_session_commands() {
        assert_eq!(
            parse_session_command("/session new"),
            Some(Ok(SessionCommand::New))
        );
        assert_eq!(
            parse_session_command("/session id=issue-42"),
            Some(Ok(SessionCommand::Switch("issue-42".to_string())))
        );
        assert_eq!(
            parse_session_command("/session clean"),
            Some(Ok(SessionCommand::Clean))
        );
        assert_eq!(
            parse_session_command("/clean"),
            Some(Ok(SessionCommand::Clean))
        );
        assert_eq!(
            parse_session_command("/session -h"),
            Some(Ok(SessionCommand::Help))
        );
        assert_eq!(
            parse_session_command("/session --help"),
            Some(Ok(SessionCommand::Help))
        );
    }

    #[test]
    fn rejects_invalid_session_commands() {
        for input in [
            "/session",
            "/session id=",
            "/session id=one two",
            "/session clean now",
        ] {
            assert!(
                matches!(parse_session_command(input), Some(Err(_))),
                "{input}"
            );
        }
        assert_eq!(parse_session_command("/sessions"), None);
    }

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

    #[tokio::test]
    async fn uninstall_removes_only_the_executable() {
        let dir = std::env::temp_dir().join(format!(
            "fae-uninstall-{}-{}",
            std::process::id(),
            wd_tools::uuid::v4()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let executable = dir.join("fae");
        let config = dir.join("fae_config.json");
        tokio::fs::write(&executable, "binary").await.unwrap();
        tokio::fs::write(&config, "{}").await.unwrap();

        uninstall(&executable).await.unwrap();

        assert!(!executable.exists());
        assert!(config.exists());
        tokio::fs::remove_dir_all(dir).await.unwrap();
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
        let agent_builder = SingleAgentPlanBuilder::with_home_dir(loader.home_dir());
        let engine = build_engine(loader, agent_builder).await;
        let (env, _) = WorkflowEnv::new("terminal-smoke-test", json!({"value": 42}));
        let (_, output) = engine.invoke::<_, Value>(env).await.unwrap();
        engine.exit().await.unwrap();

        assert_eq!(output, json!({"value": 42}));
    }
}
