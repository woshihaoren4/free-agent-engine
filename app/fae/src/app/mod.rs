mod agent;
pub mod args;
pub mod client;
pub mod daemon;
mod output;
pub mod paths;
mod workflow;

use crate::protocol::{ControlMessage, InitArgsDto, ServerMessage};
use args::{Cli, Command, ServerCommand};

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let home = paths::home(cli.fae_home)?;
    match cli.command {
        Some(Command::Lark(args)) => {
            use args::LarkCommand;
            let explicit_workspace = std::env::args_os().any(|arg| {
                arg == "--workspace" || arg.to_string_lossy().starts_with("--workspace=")
            });
            if explicit_workspace
                && !matches!(
                    args.command,
                    LarkCommand::Init { .. } | LarkCommand::Stop | LarkCommand::Status
                )
            {
                let config = crate::lark::config::Config::load(&home)?;
                anyhow::ensure!(
                    paths::workspace(cli.workspace.clone())? == config.workspace,
                    "--workspace conflicts with lark.config workspace"
                );
            }
            match args.command {
                LarkCommand::Init { force } => {
                    crate::lark::config::Config::init(
                        &home,
                        paths::workspace(cli.workspace)?,
                        force,
                    )?;
                    println!(
                        "Created {}. Fill in credentials and run fae lark check.",
                        home.join("lark.config").display()
                    );
                }
                LarkCommand::Check { online } => {
                    let config = crate::lark::config::Config::load(&home)?;
                    config.check_agent(&home).await?;
                    if online {
                        crate::lark::check_online(config).await?;
                    }
                    println!(
                        "Lark configuration valid{}",
                        if online {
                            "; bot authentication verified"
                        } else {
                            ""
                        }
                    );
                }
                LarkCommand::Show => println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &crate::lark::config::Config::load(&home)?.redacted()
                    )?
                ),
                LarkCommand::Start { foreground: true } => return crate::lark::run(home).await,
                LarkCommand::Start { foreground: false } => {
                    println!("{}", crate::lark::lifecycle::start(&home).await?)
                }
                LarkCommand::Status => match crate::lark::lifecycle::status(&home).await? {
                    Some(status) => println!("{}", serde_json::to_string_pretty(&status)?),
                    None => println!("Lark gateway not running"),
                },
                LarkCommand::Stop => println!(
                    "{}",
                    if crate::lark::lifecycle::stop(&home).await? {
                        "Lark gateway stopped"
                    } else {
                        "Lark gateway not running"
                    }
                ),
            }
            Ok(())
        }
        Some(Command::Server(args)) => match args.command {
            ServerCommand::Start {
                address,
                port,
                foreground: true,
            } => crate::server::run(home, address, port).await,
            ServerCommand::Start {
                address,
                port,
                foreground: false,
            } => {
                let config = daemon::start(&home, address, port, true).await?;
                println!(
                    "Service running: PID {} {}:{}\nConfig: {}",
                    config.pid,
                    config.address,
                    config.port,
                    home.join("server.config").display()
                );
                Ok(())
            }
            ServerCommand::Stop => {
                println!(
                    "{}",
                    if daemon::stop(&home).await? {
                        "Service stopped"
                    } else {
                        "Service not running"
                    }
                );
                Ok(())
            }
            ServerCommand::Status => {
                if let Some(config) = daemon::discover(&home).await? {
                    println!(
                        "PID {} {}:{} version {}\nConfig: {}",
                        config.pid,
                        config.address,
                        config.port,
                        config.app_version,
                        home.join("server.config").display()
                    );
                    println!(
                        "{}",
                        serde_json::to_string_pretty(
                            &client::control(&home, &config, ControlMessage::Status).await?
                        )?
                    );
                } else {
                    println!("Service not running");
                }
                Ok(())
            }
            ServerCommand::Show => {
                if let Some(config) = daemon::discover(&home).await? {
                    println!(
                        "PID: {}\nAddress: {}\nPort: {}\nVersion: {}\nProtocol: {}\nStarted: {}\nInstance: {}\nExecutable: {}\nConfig: {}",
                        config.pid,
                        config.address,
                        config.port,
                        config.app_version,
                        config.protocol_version,
                        config.started_at,
                        config.instance_id,
                        config.executable.display(),
                        home.join("server.config").display()
                    );
                } else {
                    println!("Service not running");
                }
                Ok(())
            }
        },
        Some(Command::Uninstall) => {
            daemon::ensure(&home).await?;
            daemon::stop(&home).await?;
            let executable = std::env::current_exe()?;
            tokio::fs::remove_file(&executable).await?;
            println!("Removed {}.", executable.display());
            Ok(())
        }
        Some(Command::Init(args)) => {
            let config = daemon::ensure(&home).await?;
            let response = client::control(
                &home,
                &config,
                ControlMessage::Initialize(InitArgsDto {
                    agent_id: args.agent_id.clone(),
                    model: args.model,
                    force: args.force,
                }),
            )
            .await?;
            let ServerMessage::Initialized {
                config_path,
                skill_count,
            } = response
            else {
                anyhow::bail!("Unexpected init response");
            };
            println!(
                "Initialized agent `{}`.\nConfig: {}\nEnabled {} tools and {} installed skills.",
                args.agent_id,
                config_path.display(),
                fae_engine::DEFAULT_TOOL_NAMES.len(),
                skill_count
            );
            Ok(())
        }
        Some(Command::Agent(args)) => {
            let config = daemon::ensure(&home).await?;
            agent::run(
                args,
                &home,
                &config,
                paths::workspace(cli.workspace)?,
                cli.color,
                cli.no_alt_screen,
            )
            .await
        }
        Some(Command::Workflow(args)) => {
            let config = daemon::ensure(&home).await?;
            workflow::run(
                args,
                &home,
                &config,
                paths::workspace(cli.workspace)?,
                cli.color,
                cli.no_alt_screen,
            )
            .await
        }
        None => unreachable!("default agent command is inserted before parsing"),
    }
}
