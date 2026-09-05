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
    /// Start an interactive single-agent session
    Agent(AgentArgs),
    /// Run a workflow stored in FAE_HOME/workflows
    Workflow(WorkflowArgs),
}

#[derive(Debug, Clone, Args)]
pub struct AgentArgs {
    /// Model name (defaults to FAE_DEFAULT_MODEL)
    #[arg(short, long, env = "FAE_DEFAULT_MODEL")]
    pub model: Option<String>,

    #[arg(long, default_value = "local")]
    pub user: String,

    /// Stable session identifier used for persisted chat history
    #[arg(long)]
    pub session: Option<String>,

    #[arg(
        long,
        default_value = "You are a pragmatic coding agent. Work carefully, use tools when needed, and answer concisely."
    )]
    pub system_prompt: String,

    /// Built-in tool exposed to the agent; may be repeated
    #[arg(long = "tool", default_values = ["read_file", "execute_command"])]
    pub tools: Vec<String>,

    /// Skill name or path to a SKILL.md file; may be repeated
    #[arg(long = "skill")]
    pub skills: Vec<String>,

    /// Configured MCP server name; may be repeated
    #[arg(long = "mcp-server")]
    pub mcp_servers: Vec<String>,

    #[arg(long, default_value_t = 32_000)]
    pub context_size: usize,

    #[arg(long, default_value_t = 20)]
    pub history_turns: usize,

    #[arg(long, default_value_t = 4_096)]
    pub max_completion_tokens: u32,

    #[arg(long, default_value_t = 8)]
    pub max_tool_iterations: usize,

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
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
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
            "agent" | "workflow" => return args,
            "--fae-home" | "--color" => index += 2,
            "--no-alt-screen" => index += 1,
            "--help" | "-h" | "--version" | "-V" => {
                has_root_help = true;
                break;
            }
            value if value.starts_with("--fae-home=") || value.starts_with("--color=") => {
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
    fn defaults_to_agent_mode() {
        let cli = Cli::try_parse_from(with_default_agent(
            ["fae", "review", "this"].map(Into::into),
        ))
        .unwrap();

        let Some(Command::Agent(agent)) = cli.command else {
            panic!("expected agent command");
        };
        assert_eq!(agent.prompt, ["review", "this"]);
        assert_eq!(agent.tools, ["read_file", "execute_command"]);
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
    }
}
