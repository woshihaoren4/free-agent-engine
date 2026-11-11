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
    #[serde(default)]
    pub legacy_reply_in_thread: Option<bool>,
    #[serde(default)]
    pub received_seq: u64,
    #[serde(default = "crate::protocol::legacy_mapping_version")]
    pub session_mapping_version: u16,
    #[serde(default)]
    pub inbound_thread_id: String,
    #[serde(default)]
    pub root_id: String,
    #[serde(default)]
    pub parent_id: String,
    #[serde(default)]
    pub thread_state: ThreadState,
    #[serde(default)]
    pub thread_create_uuid: String,
    #[serde(default)]
    pub thread_create_attempt: Option<i64>,
    #[serde(default)]
    pub thread_reply_message_id: String,
    #[serde(default)]
    pub thread_reply_pending_removal: bool,
    #[serde(default)]
    pub reply_anchor_message_id: String,
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
    #[serde(default)]
    pub completion_receipt: Option<String>,
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
            session_mapping_version: 1,
            thread_id: String::new(),
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
    #[test]
    fn legacy_records_without_mapping_fields_still_load_strictly() {
        let home = std::env::temp_dir().join(format!("fae-legacy-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(home.join("lark/records")).unwrap();
        let binding: SessionBinding = serde_json::from_value(serde_json::json!({
            "channel":"lark","app_id":"app","tenant_key":"tenant","user_id":"ou_a","chat_type":"group","chat_id":"chat"
        })).unwrap();
        let id = super::super::identity::receipt_id("app", "tenant", "om_input");
        let record = Record::new(
            id.clone(),
            super::super::identity::session_id(&binding),
            binding,
            "om_input".into(),
            "text".into(),
            None,
        );
        let mut old = serde_json::to_value(record).unwrap();
        for field in [
            "session_mapping_version",
            "received_seq",
            "inbound_thread_id",
            "root_id",
            "parent_id",
            "thread_state",
            "thread_create_uuid",
            "thread_create_attempt",
            "thread_reply_message_id",
            "reply_anchor_message_id",
        ] {
            old.as_object_mut().unwrap().remove(field);
        }
        old["binding"]
            .as_object_mut()
            .unwrap()
            .remove("session_mapping_version");
        old["binding"].as_object_mut().unwrap().remove("thread_id");
        let path = home.join(format!("lark/records/{id}.json"));
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(
            Store::open(&home, 7)
                .unwrap()
                .get(&id)
                .session_mapping_version,
            1
        );
        old["session_id"] = "omt_fake".into();
        std::fs::write(path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(Store::open(&home, 7).is_err());
        std::fs::remove_dir_all(home).unwrap();
    }
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    #[default]
    Unresolved,
    Creating,
    Ready,
    AcceptanceUnknown,
    Failed,
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
    pub fn discard_unsent_responses(&mut self) {
        // Keep delivery metadata for replies that may already exist remotely.
        self.responses
            .retain(|card| card.reply_id.is_some() || card.reply_attempt.is_some());
    }

    pub fn new(
        id: String,
        session_id: String,
        binding: SessionBinding,
        message_id: String,
        text: String,
        rejection: Option<String>,
    ) -> Self {
        Self {
            legacy_reply_in_thread: None,
            received_seq: 0,
            session_mapping_version: binding.session_mapping_version,
            inbound_thread_id: binding.thread_id.clone(),
            root_id: String::new(),
            parent_id: String::new(),
            thread_state: if binding.thread_id.is_empty() {
                ThreadState::Unresolved
            } else {
                ThreadState::Ready
            },
            thread_create_uuid: wd_tools::uuid::v4(),
            thread_create_attempt: None,
            thread_reply_message_id: String::new(),
            thread_reply_pending_removal: false,
            reply_anchor_message_id: message_id.clone(),
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
            completion_receipt: None,
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
    admissions: tokio::sync::Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
}
impl Store {
    pub fn open(home: &Path, retention: u64) -> anyhow::Result<Self> {
        let dir = home.join("lark/records");
        std::fs::create_dir_all(&dir)?;
        let mut records = HashMap::new();
        let now = chrono::Utc::now().timestamp_millis();
        let legacy_thread = std::fs::read(home.join("lark.config"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|config| config["group"]["reply_in_thread"].as_bool())
            .unwrap_or(false);
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.path().extension().is_none_or(|e| e != "json") {
                continue;
            }
            let mut record: Record = serde_json::from_slice(&std::fs::read(entry.path())?)?;
            if record.session_mapping_version == 1 {
                record.legacy_reply_in_thread.get_or_insert(legacy_thread);
            }
            anyhow::ensure!(
                record.id
                    == super::identity::receipt_id(
                        &record.binding.app_id,
                        &record.binding.tenant_key,
                        &record.message_id
                    )
                    && record.valid_mapping(),
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
            admissions: tokio::sync::Mutex::new(HashMap::new()),
        })
    }
    pub fn insert(&self, mut record: Record) -> anyhow::Result<bool> {
        let mut records = self.records.lock().unwrap();
        if records.contains_key(&record.id) {
            return Ok(false);
        }
        record.received_seq = records.values().map(|r| r.received_seq).max().unwrap_or(0) + 1;
        self.write(&record)?;
        records.insert(record.id.clone(), record);
        Ok(true)
    }
    fn write(&self, record: &Record) -> anyhow::Result<()> {
        anyhow::ensure!(
            record.id.len() == 64 && record.id.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid record ID"
        );
        anyhow::ensure!(record.valid_mapping(), "Invalid Lark session mapping");
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
        let old = &records[id];
        anyhow::ensure!(
            old.session_mapping_version != 2
                || old.thread_state != ThreadState::Ready
                || (old.session_id == record.session_id && old.binding == record.binding),
            "Ready thread identity cannot change"
        );
        self.write(&record)?;
        records.insert(id.into(), record.clone());
        Ok(record)
    }
    pub fn get(&self, id: &str) -> Record {
        self.records.lock().unwrap()[id].clone()
    }
    pub async fn admission(&self, id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.admissions
            .lock()
            .await
            .entry(id.to_owned())
            .or_default()
            .clone()
            .lock_owned()
            .await
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
        records.sort_by_key(|r| (r.received_seq, r.created, r.id.clone()));
        records
    }
    pub fn round_receipt(&self, record: &Record, round: &str) -> Option<String> {
        self.records
            .lock()
            .unwrap()
            .values()
            .find(|r| {
                r.id != record.id
                    && r.binding == record.binding
                    && r.session_id == record.session_id
                    && r.service_instance == record.service_instance
                    && r.stream_id == record.stream_id
                    && r.round_id.as_deref() == Some(round)
                    && r.completion_receipt.is_none()
            })
            .map(|r| r.id.clone())
    }
    pub fn completion_delivered(&self, record: &Record) -> bool {
        let records = self.records.lock().unwrap();
        record.completion_receipt.as_ref().is_none_or(|id| {
            // Missing owners have already expired from delivered-record retention.
            records.get(id).is_none_or(|r| r.state == State::Delivered)
        })
    }
    /// Root admission precedes followups even if the creation response is still in flight.
    pub fn predecessors(&self, record: &Record) -> Vec<Record> {
        let mut result: Vec<_> = self
            .records
            .lock()
            .unwrap()
            .values()
            .filter(|r| {
                r.id != record.id
                    && r.session_mapping_version == 2
                    && r.binding.app_id == record.binding.app_id
                    && r.binding.tenant_key == record.binding.tenant_key
                    && r.binding.chat_id == record.binding.chat_id
                    && r.binding.user_id == record.binding.user_id
                    && r.outcome.is_none()
                    && ((!record.inbound_thread_id.is_empty()
                        && r.message_id == record.root_id
                        && r.inbound_thread_id.is_empty())
                        || (!record.inbound_thread_id.is_empty()
                            && !record.binding.thread_id.is_empty()
                            && r.binding.thread_id == record.binding.thread_id
                            && r.received_seq < record.received_seq))
            })
            .cloned()
            .collect();
        result.sort_by_key(|r| (r.received_seq, r.created, r.id.clone()));
        result
    }

    pub fn bind_thread(
        &self,
        record: &Record,
        config: &super::config::Config,
    ) -> anyhow::Result<ThreadBinding> {
        anyhow::ensure!(
            record.thread_state == ThreadState::Ready && record.valid_mapping(),
            "Thread not ready"
        );
        let dir = self.dir.parent().unwrap().join("threads");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!(
            "{}.json",
            super::identity::worker_key(&record.binding, &config.agent_id)
        ));
        let _lock = crate::server::lifecycle::FileLock::try_acquire(&dir.join("bindings.lock"))?
            .ok_or_else(|| anyhow::anyhow!("Thread bindings busy"))?;
        let proposed = ThreadBinding {
            binding: record.binding.clone(),
            agent_id: config.agent_id.clone(),
            workspace: config.workspace.clone(),
            root_id: record.root_id.clone(),
            anchor: if record.thread_reply_message_id.is_empty() {
                record.message_id.clone()
            } else {
                record.thread_reply_message_id.clone()
            },
        };
        let binding = match std::fs::read(&path) {
            Ok(bytes) => {
                let mut saved: ThreadBinding = serde_json::from_slice(&bytes)?;
                anyhow::ensure!(
                    saved.binding == proposed.binding
                        && saved.agent_id == proposed.agent_id
                        && saved.workspace == proposed.workspace,
                    "Thread binding conflict"
                );
                anyhow::ensure!(
                    saved.root_id.is_empty()
                        || proposed.root_id.is_empty()
                        || saved.root_id == proposed.root_id,
                    "Thread root conflict"
                );
                if saved.root_id.is_empty() && !proposed.root_id.is_empty() {
                    saved.root_id = proposed.root_id.clone();
                    crate::server::lifecycle::atomic_write(
                        &path,
                        &serde_json::to_vec(&saved)?,
                        true,
                    )?;
                }
                saved
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                crate::server::lifecycle::atomic_write(
                    &path,
                    &serde_json::to_vec(&proposed)?,
                    true,
                )?;
                proposed
            }
            Err(e) => return Err(e.into()),
        };
        if !binding.root_id.is_empty() {
            let roots = self.dir.parent().unwrap().join("roots");
            std::fs::create_dir_all(&roots)?;
            let root_key = super::identity::hash(&[
                "fae-lark-root-v2",
                &binding.binding.app_id,
                &binding.binding.tenant_key,
                &binding.binding.chat_id,
                &binding.root_id,
            ]);
            let root_path = roots.join(format!("{root_key}.json"));
            let resource = super::sdk::MessageResource {
                message_id: binding.root_id.clone(),
                chat_id: binding.binding.chat_id.clone(),
                thread_id: binding.binding.thread_id.clone(),
                root_id: binding.root_id.clone(),
            };
            match std::fs::read(&root_path) {
                Ok(bytes) => {
                    let saved: super::sdk::MessageResource = serde_json::from_slice(&bytes)?;
                    anyhow::ensure!(
                        saved.thread_id == resource.thread_id
                            && saved.chat_id == resource.chat_id
                            && saved.root_id == resource.root_id,
                        "Root/thread binding conflict"
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            crate::server::lifecycle::atomic_write(
                &root_path,
                &serde_json::to_vec(&resource)?,
                true,
            )?;
        }
        Ok(binding)
    }
    pub fn root_thread(
        &self,
        record: &Record,
    ) -> anyhow::Result<Option<super::sdk::MessageResource>> {
        // A normal quoted reply can carry root_id without being a thread
        // message. Only evidence for this input itself may resolve its thread.
        let root = &record.message_id;
        let root_key = super::identity::hash(&[
            "fae-lark-root-v2",
            &record.binding.app_id,
            &record.binding.tenant_key,
            &record.binding.chat_id,
            root,
        ]);
        let path = self
            .dir
            .parent()
            .unwrap()
            .join("roots")
            .join(format!("{root_key}.json"));
        match std::fs::read(path) {
            Ok(bytes) => {
                let resource: super::sdk::MessageResource = serde_json::from_slice(&bytes)?;
                resource.validate(&record.binding.chat_id, None)?;
                anyhow::ensure!(resource.root_id == *root, "Root binding mismatch");
                Ok(Some(resource))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct ThreadBinding {
    pub binding: SessionBinding,
    pub agent_id: String,
    pub workspace: PathBuf,
    pub root_id: String,
    pub anchor: String,
}
impl Record {
    fn valid_mapping(&self) -> bool {
        match self.session_mapping_version {
            1 => {
                self.binding.session_mapping_version == 1
                    && super::identity::owns_session(&self.binding, &self.session_id)
            }
            2 => {
                self.binding.session_mapping_version == 2
                    && (self.inbound_thread_id.is_empty()
                        || self.inbound_thread_id == self.binding.thread_id)
                    && if self.thread_state == ThreadState::Ready {
                        super::identity::owns_session(&self.binding, &self.session_id)
                    } else {
                        self.session_id.is_empty() && self.binding.thread_id.is_empty()
                    }
            }
            _ => false,
        }
    }
}
