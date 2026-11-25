use super::{
    card::{Card, pages},
    store::{Record, State},
};
use fae_agent::{ExecutionId, ExecutionOutcome, SessionId, protocol::*};

pub fn ingest(record: &mut Record, message: &Message) {
    let Some(seq) = message.seq else {
        return;
    };
    if seq <= record.seq {
        return;
    }
    match &message.payload {
        MessagePayload::Event(EventPayload::OutputDelta(delta)) => {
            // complete_query results are delivered from the authoritative
            // ExecutionFinished outcome, just like the final model answer.
            if matches!(delta.source, OutputSource::PluginResult { .. }) {
                record.seq = seq;
                return;
            }
            if matches!(
                delta.source,
                OutputSource::Plugin { .. } | OutputSource::Runtime
            ) {
                let id = format!("{}:{}", delta.invocation_id, delta.output_id);
                if !record
                    .plugin_messages
                    .iter()
                    .any(|c| c.response_id.starts_with(&format!("{id}:")))
                {
                    let text = delta
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text.clone()),
                            ContentBlock::Json { value } => Some(value.to_string()),
                            ContentBlock::Resource { resource } => Some(resource.uri.clone()),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    for (index, page) in pages(&text).into_iter().enumerate() {
                        let mut card = Card::new(format!("{id}:{index}"));
                        card.text = page;
                        card.ended = true;
                        record.plugin_messages.push(card);
                    }
                }
                record.seq = seq;
                return;
            }
            for block in &delta.content {
                match block {
                    ContentBlock::Text { text } if delta.source == OutputSource::Model => {
                        let id = format!("{}:{}", delta.invocation_id, delta.output_id);
                        if record.responses.last().is_none_or(|c| c.response_id != id) {
                            if let Some(card) = record.responses.last_mut() {
                                card.ended = true;
                            }
                            record.responses.push(Card::new(id.clone()));
                        }
                        let card = record.responses.last_mut().unwrap();
                        let full = format!("{}{}", card.text, text);
                        let mut split = pages(&full).into_iter();
                        card.text = split.next().unwrap_or_default();
                        for page in split {
                            record.responses.last_mut().unwrap().ended = true;
                            let mut continuation = Card::new(id.clone());
                            continuation.text = page;
                            record.responses.push(continuation);
                        }
                    }
                    _ => {}
                }
            }
        }
        MessagePayload::Event(EventPayload::ExecutionFinished(finished)) => {
            record.state = State::Delivering;
            match &finished.outcome {
                ExecutionOutcome::Succeeded { result } => {
                    record.outcome = Some("succeeded".into());
                    let parts = result
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            ContentBlock::Json { value } => value.as_str(),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    if !parts.is_empty() {
                        let text = parts.concat();
                        record.responses.clear();
                        for page in pages(&text) {
                            let mut card = Card::new("final-result".into());
                            card.text = page;
                            record.responses.push(card);
                        }
                    }
                }
                ExecutionOutcome::Failed { error } => {
                    record.discard_unsent_responses();
                    record.outcome = Some("failed".into());
                    record.notice = Some(format!("ERROR：{}", error.message));
                }
                ExecutionOutcome::Cancelled { .. } => {
                    record.discard_unsent_responses();
                    record.outcome = Some("cancelled".into());
                    record.notice = Some("本次执行已取消".into());
                }
            }
        }
        _ => {}
    }
    record.seq = seq;
}
pub fn recover_terminal(record: &mut Record, round_id: &str, outcome: ExecutionOutcome) {
    let message = Message::event(
        SessionId::new(record.session_id.clone()).unwrap(),
        record
            .stream_id
            .clone()
            .unwrap_or_else(|| "recovered".into()),
        record.seq + 1,
        None,
        EventPayload::ExecutionFinished(ExecutionFinished {
            execution_id: ExecutionId::new(round_id.to_string()).unwrap(),
            outcome,
        }),
    )
    .unwrap();
    ingest(record, &message);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_sources_separate_model_invocations_and_preserve_plugin_content_at_finish() {
        let binding = crate::session_host::LarkPrincipal {
            session_mapping_version: 1,
            thread_id: String::new(),
            channel: "lark".into(),
            app_id: "a".into(),
            tenant_key: "t".into(),
            user_id: "u".into(),
            chat_type: "p2p".into(),
            chat_id: "c".into(),
        };
        let mut record = Record::new(
            "id".into(),
            "s".into(),
            binding,
            "m".into(),
            "q".into(),
            None,
        );
        let delta = |seq, invocation: &str, source, text: &str| {
            Message::event(
                SessionId::new("s").unwrap(),
                "stream".into(),
                seq,
                None,
                EventPayload::OutputDelta(OutputDelta {
                    execution_id: ExecutionId::new("e").unwrap(),
                    invocation_id: fae_agent::InvocationId::new(invocation).unwrap(),
                    output_id: "shared".into(),
                    source,
                    content: vec![ContentBlock::Text { text: text.into() }],
                }),
            )
            .unwrap()
        };
        ingest(
            &mut record,
            &delta(1, "child", OutputSource::Model, "child"),
        );
        ingest(&mut record, &delta(2, "root", OutputSource::Model, "root"));
        ingest(
            &mut record,
            &delta(3, "root", OutputSource::Model, " continuation"),
        );
        ingest(
            &mut record,
            &delta(
                4,
                "root",
                OutputSource::Plugin {
                    plugin_id: "p".into(),
                },
                "plugin",
            ),
        );
        ingest(
            &mut record,
            &delta(
                5,
                "root",
                OutputSource::Plugin {
                    plugin_id: "p".into(),
                },
                "plugin",
            ),
        );
        for seq in [6, 7] {
            ingest(
                &mut record,
                &delta(
                    seq,
                    "root",
                    OutputSource::PluginResult {
                        plugin_id: "p".into(),
                    },
                    "final",
                ),
            );
        }
        assert_eq!(record.responses.len(), 2);
        assert_eq!(record.responses[0].text, "child");
        assert_eq!(record.responses[1].text, "root continuation");
        assert_eq!(record.plugin_messages.len(), 1);
        recover_terminal(
            &mut record,
            "e",
            ExecutionOutcome::Succeeded {
                result: vec![ContentBlock::Text {
                    text: "final".into(),
                }],
            },
        );
        assert_eq!(record.responses.len(), 1);
        assert_eq!(record.responses[0].text, "final");
        assert_eq!(record.plugin_messages.len(), 1);
        assert_eq!(record.plugin_messages[0].text, "plugin");
    }
    #[test]
    fn terminal_failure_discards_partial_output_and_recovery_uses_authoritative_result() {
        let binding = crate::session_host::LarkPrincipal {
            session_mapping_version: 1,
            thread_id: String::new(),
            channel: "lark".into(),
            app_id: "a".into(),
            tenant_key: "t".into(),
            user_id: "u".into(),
            chat_type: "p2p".into(),
            chat_id: "c".into(),
        };
        let mut record = Record::new(
            "id".into(),
            "s".into(),
            binding,
            "m".into(),
            "q".into(),
            None,
        );
        record.responses.push(Card::new("partial".into()));
        record.responses[0].text = "partial".into();
        recover_terminal(
            &mut record,
            "r",
            ExecutionOutcome::Failed {
                error: ErrorInfo {
                    code: "storage".into(),
                    message: "save failed".into(),
                },
            },
        );
        assert!(record.responses.is_empty());
        assert_eq!(record.notice.as_deref(), Some("ERROR：save failed"));
        record.notice = None;
        let text = "最终答复\n".repeat(5000);
        recover_terminal(
            &mut record,
            "r",
            ExecutionOutcome::Succeeded {
                result: vec![ContentBlock::Text { text: text.clone() }],
            },
        );
        assert_eq!(
            record
                .responses
                .iter()
                .map(|c| c.text.as_str())
                .collect::<String>(),
            text
        );
        assert!(record.responses.len() > 1);
        assert_eq!(record.outcome.as_deref(), Some("succeeded"));
        recover_terminal(
            &mut record,
            "r",
            ExecutionOutcome::Succeeded {
                result: vec![ContentBlock::Text {
                    text: String::new(),
                }],
            },
        );
        assert!(record.responses.iter().all(|card| card.text.is_empty()));
    }
}
