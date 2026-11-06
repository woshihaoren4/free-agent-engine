use fae_agent::{TimerDefinition, TimerOccurrence, TimerOwner};
use serde::{Deserialize, Serialize};
use std::{collections::{HashMap, HashSet}, io::Write, path::{Path, PathBuf}};

pub(crate) type PartitionKey = (String, String);
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Partition {
    pub schema_version: u32,
    pub timers: Vec<TimerDefinition>,
    pub occurrences: Vec<TimerOccurrence>,
    // Key -> original request fingerprint and immutable timer ID.
    pub idempotency: HashMap<String, (String, String)>,
}
impl Default for Partition {
    fn default() -> Self {
        Self { schema_version: 1, timers: vec![], occurrences: vec![], idempotency: HashMap::new() }
    }
}
#[derive(Debug, Default)]
pub(crate) struct Store {
    pub partitions: HashMap<PartitionKey, Partition>,
    pub corrupt: HashSet<PartitionKey>,
}
pub(crate) fn key(owner: &TimerOwner) -> anyhow::Result<PartitionKey> {
    crate::engine_rt::validate_path_segment("agent_id", &owner.agent_id)?;
    crate::engine_rt::validate_path_segment("user_id", &owner.user_id)?;
    Ok((owner.agent_id.clone(), owner.user_id.clone()))
}
fn path(home: &Path, key: &PartitionKey) -> PathBuf {
    home.join("timer").join(&key.0).join(&key.1).join("state.json")
}
pub(crate) fn load(home: &Path) -> anyhow::Result<Store> {
    let mut store = Store::default();
    let root = home.join("timer");
    if !root.exists() { return Ok(store) }
    for agent in std::fs::read_dir(root)? {
        let agent = agent?;
        if !agent.file_type()?.is_dir() { continue }
        for user in std::fs::read_dir(agent.path())? {
            let user = user?;
            if !user.file_type()?.is_dir() { continue }
            let owner = TimerOwner { agent_id: agent.file_name().to_string_lossy().into(), user_id: user.file_name().to_string_lossy().into() };
            let k = key(&owner)?;
            let p = path(home, &k);
            if !p.exists() { continue }
            let data = std::fs::read(&p)?;
            let parsed: serde_json::Value = match serde_json::from_slice(&data) {
                Ok(value) => value,
                Err(e) => { eprintln!("timer partition {} isolated: {e}", p.display()); store.corrupt.insert(k); continue }
            };
            anyhow::ensure!(parsed["schema_version"] == 1, "unsupported timer schema: {}", p.display());
            let result = (|| -> anyhow::Result<Partition> {
                let partition: Partition = serde_json::from_value(parsed)?;
                let mut ids = HashSet::new();
                for timer in &partition.timers {
                    anyhow::ensure!(timer.owner == owner && timer.execution_binding.owner == owner && ids.insert(&timer.id), "timer partition ownership or ID mismatch");
                    super::parse(&timer.schedule)?;
                    if let Some(t) = &timer.next_fire_at { super::timestamp(t)?; }
                    if let Some(t) = &timer.queued_at { super::timestamp(t)?; }
                    if let Some(t) = &timer.last_scheduled_at { super::timestamp(t)?; }
                }
                let mut occurrences = HashSet::new();
                for occurrence in &partition.occurrences {
                    anyhow::ensure!(occurrence.owner == owner && occurrence.execution_binding.owner == owner && ids.contains(&occurrence.timer_id) && occurrences.insert(&occurrence.occurrence_id), "occurrence ownership or ID mismatch");
                    super::timestamp(&occurrence.scheduled_at)?;
                    crate::engine_rt::validate_path_segment("session_id", &occurrence.session_id)?;
                }
                Ok(partition)
            })();
            match result {
                Ok(partition) => { store.partitions.insert(k, partition); }
                Err(e) => { eprintln!("timer partition {} isolated: {e}", p.display()); store.corrupt.insert(k); }
            }
        }
    }
    Ok(store)
}

pub(crate) async fn commit(home: PathBuf, key: PartitionKey, partition: Partition) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let target = path(&home, &key);
        let parent = target.parent().unwrap();
        std::fs::create_dir_all(parent)?;
        // Refuse redirected partition directories.
        for directory in [home.join("timer"), home.join("timer").join(&key.0), parent.to_path_buf()] {
            anyhow::ensure!(!std::fs::symlink_metadata(directory)?.file_type().is_symlink(), "timer storage directory cannot be a symlink");
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(&mut temp, &partition)?;
        temp.flush()?;
        temp.as_file().sync_all()?;
        temp.persist(&target).map_err(|e| e.error)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }).await?
}
