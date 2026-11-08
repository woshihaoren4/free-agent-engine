use super::registry::Key;
use crate::protocol::ProtocolError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
pub struct Receipt {
    key: Vec<String>,
    digest: String,
    pub round_id: Option<String>,
    pub uncertain: bool,
    #[serde(default)]
    pub outcome: Option<crate::protocol::RoundOutcome>,
}
fn path(home: &Path, id: &str) -> Result<PathBuf, ProtocolError> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ProtocolError::new(
            "invalid_request_id",
            "External request ID must be a SHA256 hash",
        ));
    }
    Ok(home.join("requests").join(format!("{id}.json")))
}
fn identity(key: &Key) -> Vec<String> {
    vec![
        key.kind.clone(),
        key.target.clone(),
        key.user.clone(),
        key.session.clone(),
    ]
}
pub fn read(
    home: &Path,
    id: &str,
    key: &Key,
    digest: Option<&str>,
) -> Result<Option<Receipt>, ProtocolError> {
    let path = path(home, id)?;
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ProtocolError::new("acceptance_store", e)),
    };
    let receipt: Receipt =
        serde_json::from_slice(&bytes).map_err(|e| ProtocolError::new("acceptance_store", e))?;
    if receipt.key != identity(key) || digest.is_some_and(|d| d != receipt.digest) {
        return Err(ProtocolError::new(
            "request_conflict",
            "Request is bound to different input or identity",
        ));
    }
    Ok(Some(receipt))
}
pub fn write(
    home: &Path,
    id: &str,
    key: &Key,
    digest: String,
    round_id: Option<String>,
    uncertain: bool,
) -> Result<(), ProtocolError> {
    let path = path(home, id)?;
    std::fs::create_dir_all(path.parent().unwrap())
        .map_err(|e| ProtocolError::new("acceptance_store", e))?;
    super::lifecycle::atomic_write(
        &path,
        &serde_json::to_vec(&Receipt {
            key: identity(key),
            digest,
            round_id,
            uncertain,
            outcome: None,
        })
        .unwrap(),
        true,
    )
    .map_err(|e| ProtocolError::new("acceptance_store", e))
}

pub fn finish(
    home: &Path,
    id: &str,
    key: &Key,
    outcome: crate::protocol::RoundOutcome,
) -> Result<(), ProtocolError> {
    let mut receipt = read(home, id, key, None)?
        .ok_or_else(|| ProtocolError::new("acceptance_store", "Missing terminal receipt"))?;
    receipt.outcome = Some(outcome);
    super::lifecycle::atomic_write(
        &path(home, id)?,
        &serde_json::to_vec(&receipt).unwrap(),
        true,
    )
    .map_err(|e| ProtocolError::new("acceptance_store", e))
}

pub async fn lock(home: &Path, id: &str) -> Result<super::lifecycle::FileLock, ProtocolError> {
    let file = path(home, id)?.with_extension("lock");
    std::fs::create_dir_all(file.parent().unwrap())
        .map_err(|e| ProtocolError::new("acceptance_store", e))?;
    super::lifecycle::FileLock::acquire(file)
        .await
        .map_err(|e| ProtocolError::new("acceptance_store", e))
}
