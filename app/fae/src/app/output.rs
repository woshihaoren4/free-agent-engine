use super::client::Client;
use crate::protocol::{ClientMessage, ServerMessage};
use fae_agent::{SessionEventData, SessionOutput};
use std::io::{self, Write};

#[derive(Default)]
pub struct AgentOutputStream {
    line_open: bool,
    nested_output_active: bool,
}
impl AgentOutputStream {
    pub fn write_event(
        &mut self,
        writer: &mut impl Write,
        event: &SessionOutput,
    ) -> anyhow::Result<()> {
        let nested = event.parament_plan_id.is_some();
        match event.event_data()? {
            SessionEventData::ModelOutput { content } => {
                writer.write_all(content.as_bytes())?;
                writer.flush()?;
                self.line_open = true;
                self.nested_output_active |= nested;
            }
            SessionEventData::Completed { .. } if nested && self.nested_output_active => {
                writer.write_all(b"\n")?;
                writer.flush()?;
                self.line_open = false;
                self.nested_output_active = false;
            }
            _ => {}
        }
        Ok(())
    }
    pub fn finish(&mut self, writer: &mut impl Write) -> anyhow::Result<()> {
        if self.line_open {
            writer.write_all(b"\n")?;
            writer.flush()?;
            self.line_open = false;
        }
        Ok(())
    }
}
pub async fn stream(client: &mut Client, workflow: bool) -> anyhow::Result<()> {
    let mut output = AgentOutputStream::default();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                if let Some(round_id) = client.info.status.round_id.clone() {
                    client.send(ClientMessage::Cancel { round_id }).await?;
                }
            }
            event = client.next() => match event? {
                ServerMessage::Event { output: event, .. } if !workflow => output.write_event(&mut io::stdout().lock(), &event)?,
                ServerMessage::Finished { outcome, .. } => {
                    output.finish(&mut io::stdout().lock())?;
                    let result = outcome.result()?;
                    if workflow { println!("{}", serde_json::to_string_pretty(&result)?); }
                    let _ = client.request(ClientMessage::Close { cancel_active: false }).await;
                    return Ok(());
                }
                ServerMessage::Error(error) => return Err(error.into()),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_output_includes_nested_and_top_level_agent_messages() {
        let mut writer = Vec::new();
        let mut output = AgentOutputStream::default();
        for event in [
            fae_agent::SessionEvent::nested_agent("coordinator","worker",1,"model",SessionEventData::ModelOutput { content: "planned".into() }),
            fae_agent::SessionEvent::nested_agent("coordinator","worker",1,"worker",SessionEventData::Completed { content: "planned".into() }),
            fae_agent::SessionEvent::single_agent_for("coordinator",1,"model",SessionEventData::ModelOutput { content: "finished".into() }),
        ] {
            output.write_event(&mut writer, &event.into()).unwrap();
        }
        output.finish(&mut writer).unwrap();
        assert_eq!(String::from_utf8(writer).unwrap(), "planned\nfinished\n");
    }
}
