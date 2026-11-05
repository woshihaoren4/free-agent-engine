use crate::protocol::{ProtocolError, ServerEnvelope};
use std::collections::VecDeque;

pub struct Replay {
    items: VecDeque<(ServerEnvelope, usize)>,
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
    pub fn push(&mut self, event: ServerEnvelope) {
        let bytes = serde_json::to_vec(&event).map_or(0, |v| v.len());
        self.items.push_back((event, bytes));
        self.bytes += bytes;
        while self.items.len() > 4096 || self.bytes > 16 * 1024 * 1024 {
            if let Some((_, size)) = self.items.pop_front() {
                self.bytes -= size;
            }
        }
    }
    pub fn after(&self, seq: u64) -> Result<Vec<ServerEnvelope>, ProtocolError> {
        let first = self
            .items
            .front()
            .and_then(|(e, _)| e.body.seq())
            .unwrap_or(self.seq + 1);
        if seq > self.seq || seq.saturating_add(1) < first {
            let mut error = ProtocolError::new(
                "resume_gap",
                "Output is incomplete; query persisted history and continue from latest_seq.",
            );
            error.detail = Some(serde_json::json!({"first_seq":first,"latest_seq":self.seq}));
            return Err(error);
        }
        Ok(self
            .items
            .iter()
            .filter(|(e, _)| e.body.seq().is_some_and(|s| s > seq))
            .map(|(e, _)| e.clone())
            .collect())
    }
}

pub struct Requests(VecDeque<(String, String, ServerEnvelope)>);
impl Requests {
    pub fn new() -> Self {
        Self(VecDeque::new())
    }
    pub fn get(&self, id: &str, body: &str) -> Result<Option<ServerEnvelope>, ProtocolError> {
        match self.0.iter().find(|(cached, _, _)| cached == id) {
            Some((_, original, response)) if original == body => Ok(Some(response.clone())),
            Some(_) => Err(ProtocolError::new(
                "request_id_conflict",
                "Request ID was already used with different content",
            )),
            None => Ok(None),
        }
    }
    pub fn push(&mut self, id: String, body: String, response: ServerEnvelope) {
        self.0.push_back((id, body, response));
        if self.0.len() > 256 {
            self.0.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ServerMessage, RoundOutcome};
    fn event(seq: u64) -> ServerEnvelope {
        ServerEnvelope::new("instance", Some("stream"), None, ServerMessage::Finished { seq, round_id: format!("round-{seq}"),
            outcome: RoundOutcome::Cancelled { reason: "test".into() } })
    }
    #[test]
    fn replay_is_ordered_and_reports_eviction_and_future_sequences() {
        let mut replay = Replay::new();
        for _ in 0..4100 { let seq = replay.next_seq(); replay.push(event(seq)); }
        assert!(replay.after(0).is_err());
        assert!(replay.after(4101).is_err());
        let missed = replay.after(4098).unwrap();
        assert_eq!(missed.iter().map(|e| e.body.seq().unwrap()).collect::<Vec<_>>(), [4099, 4100]);
        assert_eq!(replay.after(4).unwrap().len(), 4096);
    }
    #[test]
    fn request_dedup_rejects_conflicts_and_has_a_bounded_lifetime() {
        let mut requests = Requests::new();
        requests.push("same".into(), "input".into(), event(1));
        assert!(requests.get("same", "input").unwrap().is_some());
        assert_eq!(requests.get("same", "clean").unwrap_err().code, "request_id_conflict");
        for index in 0..256 { requests.push(index.to_string(), "input".into(), event(index)); }
        assert!(requests.get("same", "input").unwrap().is_none());
    }
}
