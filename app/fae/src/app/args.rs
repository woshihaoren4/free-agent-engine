use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "fae",
    version,
    about = "Codex-style terminal client for Free Agent Engine"
)]
pub struct Cli {
    #[arg(long, global = true, env = "FAE_HOST")]
    pub fae_home: Option<PathBuf>,

    /// Session workspace override (defaults to the current directory)
    #[arg(long, global = true, default_value = ".")]
    pub workspace: PathBuf,

    #[arg(long, global = true, default_value = "auto")]
    pub color: ColorChoice,

    /// Keep terminal scrollback instead of using the alternate screen
    #[arg(long, global = true)]
    pub no_alt_screen: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the Feishu agent gateway (never started by default)
    Lark(LarkArgs),
    /// Manage the background service for this FAE home
    Server(ServerArgs),
    /// Initialize an agent with all installed tools and skills
    Init(InitArgs),
    /// Remove the currently running fae executable
    Uninstall,
    /// Run a single-agent conversation
    Agent(AgentArgs),
    /// Run a workflow stored in FAE_HOME/workflows
    Workflow(WorkflowArgs),
}

#[derive(Debug, Args)]
pub struct LarkArgs {
    #[command(subcommand)]
    pub command: LarkCommand,
}
#[derive(Debug, Subcommand)]
pub enum LarkCommand {
    Init {
        #[arg(long)]
        force: bool,
    },
    Check {
        #[arg(long)]
        online: bool,
    },
    Show,
    Start {
        #[arg(long)]
        foreground: bool,
    },
    Status,
    Stop,
}

#[derive(Debug, Args)]
pub struct ServerArgs {
    #[command(subcommand)]
    pub command: ServerCommand,
}

#[derive(Debug, Subcommand)]
pub enum ServerCommand {
    /// Start the service and wait until it is ready
    Start {
        #[arg(long, default_value = "127.0.0.1")]
        address: std::net::IpAddr,
        #[arg(long, default_value_t = crate::protocol::DEFAULT_PORT, value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
        #[arg(long, hide = true)]
        foreground: bool,
    },
    /// Stop the service and wait for cleanup
    Stop,
    /// Inspect the service without starting it
    Status,
    /// Show the service PID, listening address and other connection information
    Show,
}

#[derive(Debug, Clone, Args)]
pub struct InitArgs {
    /// Agent ID written to FAE_HOME/agents
    #[arg(long, default_value = "fae")]
    pub agent_id: String,

    /// Model used by the initialized agent
    #[arg(long, env = "FAE_DEFAULT_MODEL", default_value = "gpt-4o-mini")]
    pub model: String,

    /// Replace an existing agent config
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Args)]
pub struct AgentArgs {
    /// Agent ID loaded from FAE_HOME/agents
    #[arg(long, default_value = "fae")]
    pub agent_id: String,

    /// Session ID used to load and save conversation history
    #[arg(long)]
    pub session_id: Option<String>,

    /// Explicit agent config path; requires --agent-prompt
    #[arg(long, requires = "agent_prompt")]
    pub agent_config: Option<PathBuf>,

    /// Explicit agent prompt path; requires --agent-config
    #[arg(long, requires = "agent_config")]
    pub agent_prompt: Option<PathBuf>,

    /// Optional first message; omit it to start at the prompt
    #[arg(value_name = "PROMPT", num_args = 0.., trailing_var_arg = true)]
    pub prompt: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct WorkflowArgs {
    /// Workflow ID loaded from <FAE_HOME>/workflows/<ID>.json
    pub id: String,

    /// JSON value, or @path to read JSON from a file
    #[arg(short, long, default_value = "{}")]
    pub input: String,

    /// Show workflow progress in the interactive terminal UI
    #[arg(long)]
    pub interactive: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    pub fn enabled(self) -> bool {
        match self {
            Self::Auto => std::env::var_os("NO_COLOR").is_none(),
            Self::Always => true,
            Self::Never => false,
        }
    }
}

pub fn parse() -> Cli {
    Cli::parse_from(with_default_agent(std::env::args_os()))
}

fn with_default_agent(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut args: Vec<_> = args.into_iter().collect();
    let mut index = 1;
    let mut has_root_help = false;

    while index < args.len() {
        let value = args[index].to_string_lossy();
        match value.as_ref() {
            "init" | "uninstall" | "agent" | "workflow" | "server" | "lark" => return args,
            "--fae-home" | "--workspace" | "--color" => index += 2,
            "--no-alt-screen" => index += 1,
            "--help" | "-h" | "--version" | "-V" => {
                has_root_help = true;
                break;
            }
            value
                if value.starts_with("--fae-home=")
                    || value.starts_with("--workspace=")
                    || value.starts_with("--color=") =>
            {
                index += 1;
            }
            _ => break,
        }
    }

    if !has_root_help {
        args.insert(1, OsString::from("agent"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lark_commands_are_explicit_root_commands() {
        for command in ["init", "check", "show", "start", "status", "stop"] {
            let cli =
                Cli::try_parse_from(with_default_agent(["fae", "lark", command].map(Into::into)))
                    .unwrap();
            assert!(matches!(cli.command, Some(Command::Lark(_))));
        }
        let cli = Cli::try_parse_from(["fae", "lark", "check", "--online"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Lark(LarkArgs {
                command: LarkCommand::Check { online: true }
            }))
        ));
    }

    #[test]
    fn parses_server_commands_and_rejects_invalid_bind_parameters() {
        let cli = Cli::try_parse_from(["fae", "server", "start"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Server(ServerArgs {
                command: ServerCommand::Start {
                    port: 6666,
                    foreground: false,
                    ..
                }
            }))
        ));
        let cli = Cli::try_parse_from(with_default_agent(
            [
                "fae",
                "server",
                "start",
                "--address",
                "::1",
                "--port",
                "1666",
            ]
            .map(Into::into),
        ))
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Server(ServerArgs {
                command: ServerCommand::Start {
                    port: 1666,
                    foreground: false,
                    ..
                }
            }))
        ));
        for arguments in [
            vec!["fae", "server", "start", "--port", "0"],
            vec!["fae", "server", "start", "--port", "65536"],
            vec!["fae", "server", "start", "--address", "invalid"],
        ] {
            assert!(
                Cli::try_parse_from(with_default_agent(arguments.into_iter().map(Into::into)))
                    .is_err()
            );
        }
        for subcommand in ["stop", "status"] {
            assert!(matches!(
                Cli::try_parse_from(with_default_agent(
                    ["fae", "server", subcommand].map(Into::into)
                ))
                .unwrap()
                .command,
                Some(Command::Server(_))
            ));
        }
    }

    #[test]
    fn defaults_to_agent_mode() {
        let cli = Cli::try_parse_from(with_default_agent(
            ["fae", "review", "this"].map(Into::into),
        ))
        .unwrap();

        let Some(Command::Agent(agent)) = cli.command else {
            panic!("expected agent command");
        };
        assert_eq!(agent.prompt, ["review", "this"]);
        assert_eq!(agent.agent_id, "fae");
        assert_eq!(cli.workspace, PathBuf::from("."));
    }

    #[test]
    fn parses_workspace_before_default_agent_prompt() {
        let cli = Cli::try_parse_from(with_default_agent(
            ["fae", "--workspace", "~/project", "review"].map(Into::into),
        ))
        .unwrap();

        let Some(Command::Agent(agent)) = cli.command else {
            panic!("expected agent command");
        };
        assert_eq!(cli.workspace, PathBuf::from("~/project"));
        assert_eq!(agent.prompt, ["review"]);
    }

    #[test]
    fn parses_agent_session_id_and_prompt() {
        let cli = Cli::try_parse_from(with_default_agent(
            [
                "fae",
                "agent",
                "--agent-id",
                "fae-coding",
                "--session-id",
                "issue-42",
                "你好",
            ]
            .map(Into::into),
        ))
        .unwrap();

        let Some(Command::Agent(agent)) = cli.command else {
            panic!("expected agent command");
        };
        assert_eq!(agent.agent_id, "fae-coding");
        assert_eq!(agent.session_id.as_deref(), Some("issue-42"));
        assert_eq!(agent.prompt, ["你好"]);
    }

    #[test]
    fn agent_without_prompt_selects_interactive_mode() {
        let cli =
            Cli::try_parse_from(with_default_agent(["fae", "agent"].map(Into::into))).unwrap();

        let Some(Command::Agent(agent)) = cli.command else {
            panic!("expected agent command");
        };
        assert!(agent.prompt.is_empty());
    }

    #[test]
    fn parses_workflow_input() {
        let cli = Cli::try_parse_from(with_default_agent(
            [
                "fae",
                "--color",
                "never",
                "workflow",
                "release",
                "--input",
                r#"{"tag":"v1"}"#,
            ]
            .map(Into::into),
        ))
        .unwrap();

        let Some(Command::Workflow(args)) = cli.command else {
            panic!("expected workflow command");
        };
        assert_eq!(args.id, "release");
        assert_eq!(args.input, r#"{"tag":"v1"}"#);
        assert!(!args.interactive);
    }

    #[test]
    fn parses_interactive_workflow_mode() {
        let cli = Cli::try_parse_from(with_default_agent(
            ["fae", "workflow", "release", "--interactive"].map(Into::into),
        ))
        .unwrap();

        let Some(Command::Workflow(args)) = cli.command else {
            panic!("expected workflow command");
        };
        assert!(args.interactive);
    }

    #[test]
    fn parses_init_without_inserting_agent_mode() {
        let cli = Cli::try_parse_from(with_default_agent(["fae", "init"].map(Into::into))).unwrap();

        let Some(Command::Init(args)) = cli.command else {
            panic!("expected init command");
        };
        assert_eq!(args.agent_id, "fae");
        assert!(!args.model.is_empty());
        assert!(!args.force);
    }

    #[test]
    fn parses_uninstall_without_inserting_agent_mode() {
        let cli =
            Cli::try_parse_from(with_default_agent(["fae", "uninstall"].map(Into::into))).unwrap();

        assert!(matches!(cli.command, Some(Command::Uninstall)));
    }
}
