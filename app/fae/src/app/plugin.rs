use super::{
    args::{PluginArgs, PluginCommand},
    client, daemon,
};
use crate::protocol::{ControlMessage, ServerMessage};
use std::path::Path;

pub async fn run(home: &Path, args: PluginArgs) -> anyhow::Result<()> {
    match args.command {
        PluginCommand::Install { source } => {
            let id = if let Some(config) = daemon::discover(home).await? {
                match client::control(
                    home,
                    &config,
                    ControlMessage::PluginInstall {
                        source: source.canonicalize()?,
                    },
                )
                .await?
                {
                    ServerMessage::Plugins(value) => value["id"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("missing installed plugin ID"))?
                        .to_owned(),
                    ServerMessage::Error(error) => return Err(error.into()),
                    _ => anyhow::bail!("unexpected plugin response"),
                }
            } else {
                fae_agent::plugin::config::install(home, &source)?
            };
            println!("Installed {id}.");
        }
        PluginCommand::Uninstall { id } => {
            if let Some(config) = daemon::discover(home).await? {
                match client::control(
                    home,
                    &config,
                    ControlMessage::PluginUninstall { id: id.clone() },
                )
                .await?
                {
                    ServerMessage::Plugins(_) => {}
                    ServerMessage::Error(error) => return Err(error.into()),
                    _ => anyhow::bail!("unexpected plugin response"),
                }
            } else {
                fae_agent::plugin::config::uninstall(home, &id)?;
            }
            println!("Uninstalled {id}.");
        }
        PluginCommand::Show { id } => {
            let value = if let Some(config) = daemon::discover(home).await? {
                match client::control(home, &config, ControlMessage::PluginShow { id: id.clone() })
                    .await?
                {
                    ServerMessage::Plugins(value) => value,
                    ServerMessage::Error(error) => return Err(error.into()),
                    _ => anyhow::bail!("unexpected plugin response"),
                }
            } else {
                fae_agent::plugin::PluginManager::show(home, &id)?
            };
            if id == "*" {
                let mut records = value.as_object().unwrap().values().collect::<Vec<_>>();
                records.sort_by_key(|record| record["order"].as_u64().unwrap_or(0));
                println!("ORDER\tID\tINSTALLED\tENABLED\tLOADED");
                for record in records {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        record["order"],
                        record["id"].as_str().unwrap_or(""),
                        record["installed"],
                        record["enabled"],
                        record["loaded"]
                    );
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&value)?);
            }
        }
    }
    Ok(())
}
