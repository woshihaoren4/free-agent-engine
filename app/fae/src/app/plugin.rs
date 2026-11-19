use super::{args::PluginArgs, client, daemon};
use crate::protocol::{ControlMessage, ServerMessage};
use std::path::Path;

pub async fn run(home: &Path, args: PluginArgs) -> anyhow::Result<()> {
    let value = if args.show {
        if let Some(config) = daemon::discover(home).await? {
            match client::control(home, &config, ControlMessage::PluginShow { id: args.id }).await?
            {
                ServerMessage::Plugins(value) => value,
                ServerMessage::Error(error) => return Err(error.into()),
                _ => anyhow::bail!("unexpected plugin response"),
            }
        } else {
            fae_agent::plugin::PluginManager::show(home, &args.id)?
        }
    } else {
        fae_agent::plugin::validate_id(&args.id)?;
        let config = daemon::ensure(home).await?;
        match client::control(home, &config, ControlMessage::PluginLoad { id: args.id }).await? {
            ServerMessage::Plugins(value) => value,
            ServerMessage::Error(error) => return Err(error.into()),
            _ => anyhow::bail!("unexpected plugin response"),
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
