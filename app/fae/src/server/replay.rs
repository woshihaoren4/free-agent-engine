use crate::protocol::ProtocolError;
use fae_agent::protocol::{Message, ResponseMessage};
use std::collections::VecDeque;

pub struct Replay {
    items: VecDeque<(Message, usize)>,
    bytes: usize,
    pub seq: u64,
}
impl Replay {
    pub fn new() -> Self {
        Self {
            items: VecDeque::new(),
            bytes: 0,
            seq: 0,
        }
    }
    pub fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }
    pub fn push(&mut self, event: Message) {
        let bytes = serde_json::to_vec(&event).map_or(0, |v| v.len());
        self.items.push_back((event, bytes));
        self.bytes += bytes;
        while self.items.len() > 4096 || self.bytes > 16 * 1024 * 1024 {
            if let Some((_, size)) = self.items.pop_front() {
                self.bytes -= size;
            }
        }
    }
    pub fn after(&self, seq: u64) -> Result<Vec<Message>, ProtocolError> {
        let first = self
            .items
            .front()
            .and_then(|(e, _)| e.seq)
            .unwrap_or(self.seq + 1);
        if seq > self.seq || seq.saturating_add(1) < first {
            return Err(ProtocolError::new(
                "resume_gap",
                format!(
                    "Output incomplete; first_seq={first}, snapshot_seq={}",
                    self.seq
                ),
            ));
        }
        Ok(self
            .items
            .iter()
            .filter(|(e, _)| e.seq.is_some_and(|s| s > seq))
            .map(|(e, _)| e.clone())
            .collect())
    }
}
pub struct Requests(VecDeque<(String, String, ResponseMessage)>);
impl Requests {
    pub fn new() -> Self {
        Self(VecDeque::new())
    }
    pub fn get(&self, id: &str, body: &str) -> Result<Option<ResponseMessage>, ProtocolError> {
        match self.0.iter().find(|(cached, _, _)| cached == id) {
            Some((_, original, response)) if original == body => Ok(Some(response.clone())),
            Some(_) => Err(ProtocolError::new(
                "request_id_conflict",
                "Request ID already used with different content",
            )),
            None => Ok(None),
        }
    }
    pub fn push(&mut self, id: String, body: String, response: ResponseMessage) {
        self.0.push_back((id, body, response));
        if self.0.len() > 256 {
            self.0.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::{ExecutionId, RequestId, SessionId, protocol::*};
    fn event(seq: u64) -> Message {
        Message::event(
            SessionId::new("session").unwrap(),
            "stream".into(),
            seq,
            None,
            EventPayload::ExecutionFinished(ExecutionFinished {
                execution_id: ExecutionId::new(format!("execution-{seq}")).unwrap(),
                outcome: fae_agent::ExecutionOutcome::Cancelled {
                    reason: "test".into(),
                },
            }),
        )
        .unwrap()
    }
    #[test]
    fn replay_is_ordered_and_rejects_evicted_and_future_sequences() {
        let mut replay = Replay::new();
        for _ in 0..4100 {
            let seq = replay.next_seq();
            replay.push(event(seq));
        }
        assert!(replay.after(0).is_err());
        assert!(replay.after(4101).is_err());
        assert_eq!(
            replay
                .after(4098)
                .unwrap()
                .iter()
                .map(|e| e.seq.unwrap())
                .collect::<Vec<_>>(),
            [4099, 4100]
        );
        assert_eq!(replay.after(4).unwrap().len(), 4096);
    }
    #[test]
    fn request_retention_is_bounded_and_rejects_reused_ids_with_other_content() {
        let request = Message::request(
            RequestId::new("r").unwrap(),
            None,
            RequestPayload::EngineStatus(Empty {}),
        )
        .unwrap();
        let response = ResponseMessage::error(&request, ResponseCode::Conflict, "test").unwrap();
        let mut requests = Requests::new();
        requests.push("same".into(), "get".into(), response.clone());
        assert!(requests.get("same", "get").unwrap().is_some());
        assert_eq!(
            requests.get("same", "clear").unwrap_err().code,
            "request_id_conflict"
        );
        for index in 0..256 {
            requests.push(index.to_string(), "get".into(), response.clone());
        }
        assert!(requests.get("same", "get").unwrap().is_none());
    }
}
