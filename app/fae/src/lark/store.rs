use super::card::Card;
use crate::protocol::SessionBinding;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub session_id: String,
    pub binding: SessionBinding,
    pub message_id: String,
    pub text: String,
    pub rejection: Option<String>,
    pub state: State,
    pub reaction_id: Option<String>,
    pub service_instance: Option<String>,
    pub stream_id: Option<String>,
    pub round_id: Option<String>,
    pub seq: u64,
    pub responses: Vec<Card>,
    pub outcome: Option<String>,
    pub notice: Option<String>,
    pub notice_uuid: String,
    #[serde(default)]
    pub notice_attempt: Option<i64>,
    pub notice_sent: bool,
    pub created: i64,
    pub delivered_at: Option<i64>,
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_messages_and_delivery_survive_restart() {
        let home = std::env::temp_dir().join(format!("fae-receipt-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        let binding = SessionBinding {
            channel: "lark".into(),
            app_id: "app".into(),
            tenant_key: "tenant".into(),
            user_id: "u".into(),
            chat_id: "chat".into(),
            chat_type: "p2p".into(),
        };
        let id = super::super::identity::receipt_id("app", "tenant", "message");
        let record = Record::new(
            id.clone(),
            super::super::identity::session_id(&binding),
            binding,
            "message".into(),
            "input".into(),
            None,
        );
        let store = Store::open(&home, 7).unwrap();
        assert!(store.insert(record.clone()).unwrap());
        assert!(!store.insert(record.clone()).unwrap());
        store
            .update(&id, |r| {
                r.state = State::Submitting;
                r.stream_id = Some("stream".into());
            })
            .unwrap();
        drop(store);
        let store = Store::open(&home, 7).unwrap();
        assert!(store.get(&id).state == State::Submitting);
        assert_eq!(store.get(&id).stream_id.as_deref(), Some("stream"));
        assert!(!store.insert(record).unwrap());
        std::fs::remove_dir_all(home).unwrap();
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Received,
    Submitting,
    Running,
    Delivering,
    Delivered,
    AcceptanceUnknown,
}
impl Record {
    pub fn new(
        id: String,
        session_id: String,
        binding: SessionBinding,
        message_id: String,
        text: String,
        rejection: Option<String>,
    ) -> Self {
        Self {
            id,
            session_id,
            binding,
            message_id,
            text,
            rejection,
            state: State::Received,
            reaction_id: None,
            service_instance: None,
            stream_id: None,
            round_id: None,
            seq: 0,
            responses: Vec::new(),
            outcome: None,
            notice: None,
            notice_uuid: wd_tools::uuid::v4(),
            notice_attempt: None,
            notice_sent: false,
            created: chrono::Utc::now().timestamp_millis(),
            delivered_at: None,
            last_error: None,
        }
    }
}
pub struct Store {
    dir: PathBuf,
    records: Mutex<HashMap<String, Record>>,
}
impl Store {
    pub fn open(home: &Path, retention: u64) -> anyhow::Result<Self> {
        let dir = home.join("lark/records");
        std::fs::create_dir_all(&dir)?;
        let mut records = HashMap::new();
        let now = chrono::Utc::now().timestamp_millis();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.path().extension().is_none_or(|e| e != "json") {
                continue;
            }
            let record: Record = serde_json::from_slice(&std::fs::read(entry.path())?)?;
            anyhow::ensure!(
                record.id
                    == super::identity::receipt_id(
                        &record.binding.app_id,
                        &record.binding.tenant_key,
                        &record.message_id
                    )
                    && record.session_id == super::identity::session_id(&record.binding),
                "Corrupt Lark record identity"
            );
            if record
                .delivered_at
                .is_some_and(|t| now - t > retention as i64 * 86400000)
            {
                std::fs::remove_file(entry.path())?;
            } else {
                records.insert(record.id.clone(), record);
            }
        }
        Ok(Self {
            dir,
            records: Mutex::new(records),
        })
    }
    pub fn insert(&self, record: Record) -> anyhow::Result<bool> {
        let mut records = self.records.lock().unwrap();
        if records.contains_key(&record.id) {
            return Ok(false);
        }
        self.write(&record)?;
        records.insert(record.id.clone(), record);
        Ok(true)
    }
    fn write(&self, record: &Record) -> anyhow::Result<()> {
        anyhow::ensure!(
            record.id.len() == 64 && record.id.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid record ID"
        );
        crate::server::lifecycle::atomic_write(
            &self.dir.join(format!("{}.json", record.id)),
            &serde_json::to_vec(record)?,
            true,
        )
    }
    pub fn update(&self, id: &str, change: impl FnOnce(&mut Record)) -> anyhow::Result<Record> {
        let mut records = self.records.lock().unwrap();
        let mut record = records
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("Unknown receipt"))?
            .clone();
        change(&mut record);
        self.write(&record)?;
        records.insert(id.into(), record.clone());
        Ok(record)
    }
    pub fn get(&self, id: &str) -> Record {
        self.records.lock().unwrap()[id].clone()
    }
    pub fn pending(&self) -> Vec<Record> {
        let mut records: Vec<_> = self
            .records
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.state != State::Delivered)
            .cloned()
            .collect();
        records.sort_by_key(|r| r.created);
        records
    }
}
