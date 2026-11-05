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
