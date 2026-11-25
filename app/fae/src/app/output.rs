use super::client::Client;
use fae_agent::protocol::{ContentBlock, EventPayload, MessagePayload, OutputDelta, OutputSource};
use std::io::{self, Write};

#[derive(Default)]
pub struct AgentOutputStream {
    line_open: bool,
    output_id: Option<String>,
    plugin_messages: std::collections::HashSet<String>,
}
impl AgentOutputStream {
    pub fn write_delta(
        &mut self,
        writer: &mut impl Write,
        delta: &OutputDelta,
        workflow: bool,
    ) -> anyhow::Result<()> {
        if matches!(delta.source, OutputSource::PluginResult { .. }) {
            let id = format!("{}:{}", delta.invocation_id, delta.output_id);
            if !self.plugin_messages.insert(id) {
                return Ok(());
            }
        }
        if let OutputSource::Plugin { plugin_id } = &delta.source {
            let id = format!("{}:{}", delta.invocation_id, delta.output_id);
            if self.plugin_messages.insert(id) {
                self.finish(writer)?;
                for block in &delta.content {
                    let text = match block {
                        ContentBlock::Text { text } => text.clone(),
                        ContentBlock::Json { value } => value.to_string(),
                        ContentBlock::Resource { resource } => resource.uri.clone(),
                    };
                    writeln!(writer, "[plugin:{plugin_id}] {text}")?;
                }
                writer.flush()?;
            }
            return Ok(());
        }
        for block in &delta.content {
            match block {
                ContentBlock::Text { text } if !workflow => {
                    if self.line_open && self.output_id.as_ref() != Some(&delta.output_id) {
                        self.finish(writer)?;
                    }
                    writer.write_all(text.as_bytes())?;
                    writer.flush()?;
                    self.line_open = true;
                    self.output_id = Some(delta.output_id.clone());
                }
                _ => {}
            }
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
            _ = tokio::signal::ctrl_c() => { let active = client.active(); client.stop_round(active).await?; }
            event = client.next() => match event?.payload {
                MessagePayload::Event(EventPayload::OutputDelta(delta)) => output.write_delta(&mut io::stdout().lock(), &delta, workflow)?,
                MessagePayload::Event(EventPayload::ExecutionFinished(finished)) => {
                    output.finish(&mut io::stdout().lock())?;
                    let result = super::client::outcome_result(finished.outcome)?;
                    if workflow { println!("{}", serde_json::to_string_pretty(&result)?); }
                    let _ = client.close().await;
                    return Ok(());
                }
                _ => {}
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plugin_result_prints_once_without_notification_prefix() {
        let mut writer = Vec::new();
        let mut output = AgentOutputStream::default();
        let delta = OutputDelta {
            execution_id: fae_agent::ExecutionId::new("execution").unwrap(),
            invocation_id: fae_agent::InvocationId::new("invocation").unwrap(),
            output_id: "result".into(),
            source: OutputSource::PluginResult {
                plugin_id: "byte_agent_plan".into(),
            },
            content: vec![ContentBlock::Text {
                text: "model table".into(),
            }],
        };
        output.write_delta(&mut writer, &delta, false).unwrap();
        output.write_delta(&mut writer, &delta, false).unwrap();
        output.finish(&mut writer).unwrap();
        assert_eq!(String::from_utf8(writer).unwrap(), "model table\n");
    }

    #[test]
    fn public_deltas_separate_outputs_and_deduplicate_plugin_messages() {
        let mut writer = Vec::new();
        let mut output = AgentOutputStream::default();
        let delta = |id: &str, content| OutputDelta {
            execution_id: fae_agent::ExecutionId::new("execution").unwrap(),
            invocation_id: fae_agent::InvocationId::new(id.to_string()).unwrap(),
            output_id: id.into(),
            source: if id == "plugin" {
                OutputSource::Plugin {
                    plugin_id: "b".into(),
                }
            } else {
                OutputSource::Model
            },
            content,
        };
        for event in [
            delta(
                "child",
                vec![ContentBlock::Text {
                    text: "planned".into(),
                }],
            ),
            delta(
                "root",
                vec![ContentBlock::Text {
                    text: "finished".into(),
                }],
            ),
            delta(
                "plugin",
                vec![ContentBlock::Text {
                    text: "done".into(),
                }],
            ),
            delta(
                "plugin",
                vec![ContentBlock::Text {
                    text: "done".into(),
                }],
            ),
        ] {
            output.write_delta(&mut writer, &event, false).unwrap();
        }
        output.finish(&mut writer).unwrap();
        assert_eq!(
            String::from_utf8(writer).unwrap(),
            "planned\nfinished\n[plugin:b] done\n"
        );
    }
}
