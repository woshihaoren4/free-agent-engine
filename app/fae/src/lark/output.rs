use super::{
    card::{Card, pages},
    store::{Record, State},
};
use crate::protocol::{RoundOutcome, ServerMessage};

pub fn ingest(record: &mut Record, message: &ServerMessage) {
    match message {
        ServerMessage::Event { seq, output, .. } => {
            if *seq <= record.seq {
                return;
            }
            if output.parament_plan_id.is_none() && output.node_id.is_none() {
                match output.input_type.as_str() {
                    "model_response_started" => {
                        // A previous Final response can become intermediate if steering continued.
                        for card in &mut record.responses {
                            if !card.closed {
                                card.ended = true;
                            }
                        }
                        if let Some(id) = output.output["response_id"].as_str() {
                            record.responses.push(Card::new(id.into()));
                        }
                    }
                    "model_output" => {
                        if let Some(text) = output.output["content"].as_str() {
                            if let Some(card) = record.responses.last_mut() {
                                let full = format!("{}{}", card.text, text);
                                let mut split = pages(&full).into_iter();
                                card.text = split.next().unwrap_or_default();
                                let response_id = card.response_id.clone();
                                for page in split {
                                    record.responses.last_mut().unwrap().ended = true;
                                    let mut continuation = Card::new(response_id.clone());
                                    continuation.text = page;
                                    record.responses.push(continuation);
                                }
                            }
                        }
                    }
                    "model_response_ended" => {
                        let disposition = output.output["disposition"].as_str().unwrap_or_default();
                        if matches!(disposition, "tool_calls" | "continue" | "discarded") {
                            if let Some(card) = record.responses.last_mut() {
                                card.ended = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            record.seq = *seq;
        }
        ServerMessage::Finished { seq, outcome, .. } => {
            if *seq <= record.seq {
                return;
            }
            record.state = State::Delivering;
            match outcome {
                RoundOutcome::Succeeded { result } => {
                    record.outcome = Some("succeeded".into());
                    if record.responses.iter().all(|c| c.text.trim().is_empty()) {
                        if let Some(text) = result.as_str().filter(|t| !t.trim().is_empty()) {
                            for page in pages(text) {
                                let mut card = Card::new("final-result".into());
                                card.text = page;
                                record.responses.push(card);
                            }
                        }
                    }
                }
                RoundOutcome::Failed { message } => {
                    record.outcome = Some("failed".into());
                    record.notice = Some(format!("ERROR：{message}"));
                }
                RoundOutcome::Cancelled { .. } => {
                    record.outcome = Some("cancelled".into());
                    record.notice = Some("本次执行已取消".into());
                }
            }
            record.seq = *seq;
        }
        _ => {}
    }
}

pub fn recover_terminal(record: &mut Record, round_id: &str, outcome: RoundOutcome) {
    let mut fallback = None;
    if let RoundOutcome::Succeeded { result } = &outcome {
        if let Some(text) = result.as_str() {
            let last_id = record.responses.last().map(|c| c.response_id.as_str());
            let observed = record
                .responses
                .iter()
                .filter(|c| Some(c.response_id.as_str()) == last_id)
                .map(|c| c.text.as_str())
                .collect::<String>();
            if observed != text {
                fallback = Some(text.to_owned());
            }
        }
    }
    if fallback.is_some() {
        for card in &mut record.responses {
            card.ended = true;
        }
        record.outcome = Some("succeeded".into());
        record.state = State::Delivering;
        record.notice = Some("流式内容恢复不完整，以下为最终结果".into());
        for page in pages(&fallback.unwrap()) {
            let mut card = Card::new("recovered-final".into());
            card.text = page;
            card.degraded = true;
            record.responses.push(card);
        }
        record.seq += 1;
    } else {
        ingest(
            record,
            &ServerMessage::Finished {
                seq: record.seq + 1,
                round_id: round_id.into(),
                outcome,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SessionBinding;
    use fae_agent::SessionOutput;
    use serde_json::json;
    fn event(seq: u64, kind: &str, data: serde_json::Value) -> ServerMessage {
        ServerMessage::Event {
            seq,
            round_id: "r".into(),
            output: SessionOutput {
                parament_plan_id: None,
                node_id: None,
                plan_id: None,
                agent_name: None,
                runtime_id: None,
                input_type: kind.into(),
                output: data,
            },
        }
    }
    #[test]
    fn hides_tools_and_waits_for_round_success() {
        let binding = SessionBinding {
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
        for message in [
            event(1, "model_response_started", json!({"response_id":"one"})),
            event(2, "model_output", json!({"content":"中间"})),
            event(3, "tool_output", json!({"output":"secret"})),
            event(
                4,
                "model_response_ended",
                json!({"disposition":"tool_calls"}),
            ),
            event(5, "model_response_started", json!({"response_id":"two"})),
            event(6, "model_output", json!({"content":"最终"})),
            event(7, "model_response_ended", json!({"disposition":"final"})),
        ] {
            ingest(&mut record, &message);
        }
        assert_eq!(record.responses.len(), 2);
        assert_eq!(record.responses[0].text, "中间");
        assert!(record.responses[0].ended);
        assert!(!record.responses[1].ended);
        assert!(record.outcome.is_none());
        ingest(
            &mut record,
            &ServerMessage::Finished {
                seq: 8,
                round_id: "r".into(),
                outcome: RoundOutcome::Failed {
                    message: "save failed".into(),
                },
            },
        );
        assert_eq!(record.outcome.as_deref(), Some("failed"));
    }
}
