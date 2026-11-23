use super::{
    args::{PluginArgs, PluginCommand},
    client, daemon,
};
use fae_agent::protocol::*;
use std::path::Path;

pub async fn run(home: &Path, args: PluginArgs) -> anyhow::Result<()> {
    match args.command {
        PluginCommand::Install { source } => {
            let id = if let Some(config) = daemon::discover(home).await? {
                match client::control(
                    home,
                    &config,
                    RequestPayload::EnginePluginInstall(PluginInstallRequest {
                        source: source.canonicalize()?.to_string_lossy().into_owned(),
                    }),
                )
                .await?
                {
                    ResponsePayload::EnginePluginInstall(value) => value.id,
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
                    RequestPayload::EnginePluginUninstall(PluginUninstallRequest {
                        id: id.clone(),
                    }),
                )
                .await?
                {
                    ResponsePayload::EnginePluginUninstall(_) => {}
                    _ => anyhow::bail!("unexpected plugin response"),
                }
            } else {
                fae_agent::plugin::config::uninstall(home, &id)?;
            }
            println!("Uninstalled {id}.");
        }
        PluginCommand::Show { id } => {
            let value = if let Some(config) = daemon::discover(home).await? {
                match client::control(
                    home,
                    &config,
                    RequestPayload::EnginePluginShow(PluginShowRequest { id: id.clone() }),
                )
                .await?
                {
                    ResponsePayload::EnginePluginShow(value) => value
                        .plugins
                        .into_iter()
                        .map(|plugin| Ok((plugin.id.clone(), serde_json::to_value(plugin)?)))
                        .collect::<anyhow::Result<serde_json::Map<String, serde_json::Value>>>()?
                        .into(),
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
