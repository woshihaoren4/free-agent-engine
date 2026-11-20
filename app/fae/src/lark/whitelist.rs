use super::config::Config;
use crate::server::lifecycle::atomic_write;
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{Arc, RwLock},
};

/// Shared admission policy, independent of the gateway's startup configuration.
#[derive(Clone)]
pub struct Policy(Arc<RwLock<Option<Vec<String>>>>);

impl Policy {
    pub fn new(users: Option<Vec<String>>) -> Self {
        Self(Arc::new(RwLock::new(users)))
    }

    pub fn snapshot(&self) -> Option<Vec<String>> {
        self.0.read().unwrap().clone()
    }

    pub fn allows_user(&self, user: &str) -> bool {
        self.0
            .read()
            .unwrap()
            .as_ref()
            .is_none_or(|users| users.iter().any(|id| id == user))
    }

    pub fn update(&self, home: &Path, update: &Update) -> anyhow::Result<Option<Vec<String>>> {
        let mut users = self.0.write().unwrap();
        let next = update_config(home, update)?;
        *users = next.clone();
        Ok(next)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", content = "users", rename_all = "snake_case")]
pub enum Update {
    Add(Vec<String>),
    Remove(Vec<String>),
    Set(Vec<String>),
    Reload,
}

/// Patch only the whitelist, preserving credentials and all other raw settings.
/// The caller serializes updates using the gateway control loop or lark.lock.
pub fn update_config(home: &Path, update: &Update) -> anyhow::Result<Option<Vec<String>>> {
    let path = home.join("lark.config");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let config: Config = serde_json::from_value(value.clone())?;
    anyhow::ensure!(config.version == 1, "Unsupported lark.config version");
    let mut users = config.user_id_whitelist;
    match update {
        Update::Add(ids) | Update::Remove(ids) | Update::Set(ids) => {
            anyhow::ensure!(
                ids.iter().all(|id| id.starts_with("ou_")
                    && id.len() > 3
                    && !id.chars().any(char::is_whitespace)),
                "Whitelist users must be Feishu open_id values (ou_...)"
            );
            match update {
                Update::Add(_) => {
                    let users = users.get_or_insert_with(Vec::new);
                    for id in ids {
                        if !users.contains(id) {
                            users.push(id.clone());
                        }
                    }
                }
                Update::Remove(_) => {
                    let users = users.as_mut().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Cannot remove users from an unrestricted whitelist; use whitelist set"
                        )
                    })?;
                    users.retain(|id| !ids.contains(id));
                }
                Update::Set(_) => {
                    let mut unique = Vec::new();
                    for id in ids {
                        if !unique.contains(id) {
                            unique.push(id.clone());
                        }
                    }
                    users = Some(unique);
                }
                _ => unreachable!(),
            }
        }
        Update::Reload => return Ok(users),
    }
    value["user_id_whitelist"] = serde_json::to_value(&users)?;
    atomic_write(&path, &serde_json::to_vec_pretty(&value)?, true)?;
    Ok(users)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_home() -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!("fae-whitelist-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("lark.config"),
            serde_json::to_vec(&json!({
                "version":1,"app_key":"app","app_secret":"secret","tenant_key":"tenant",
                "agent_id":"fae","workspace":"~","user_id_whitelist":["ou_alice"],
                "group":{"require_mention":true,"reply_in_thread":false},
                "execution":{"session_queue_capacity":8}
            }))
            .unwrap(),
        )
        .unwrap();
        home
    }

    #[test]
    fn updates_preserve_raw_config_and_publish_only_after_success() {
        let home = test_home();
        let read = || {
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(home.join("lark.config")).unwrap(),
            )
            .unwrap()
        };
        let original = read();
        let policy = Policy::new(Some(vec!["ou_alice".into()]));
        let shared = policy.clone();
        policy
            .update(&home, &Update::Add(vec!["ou_bob".into(), "ou_bob".into()]))
            .unwrap();
        assert!(shared.allows_user("ou_bob"));
        assert_eq!(shared.snapshot().unwrap().len(), 2);
        let mut saved = read();
        saved["user_id_whitelist"] = original["user_id_whitelist"].clone();
        assert_eq!(saved, original);

        let before_error = std::fs::read(home.join("lark.config")).unwrap();
        assert!(
            policy
                .update(&home, &Update::Set(vec!["employee_id".into()]))
                .is_err()
        );
        assert!(shared.allows_user("ou_bob"));
        assert_eq!(
            std::fs::read(home.join("lark.config")).unwrap(),
            before_error
        );
        policy
            .update(&home, &Update::Remove(vec!["ou_bob".into()]))
            .unwrap();
        assert!(!shared.allows_user("ou_bob"));
        policy.update(&home, &Update::Set(vec![])).unwrap();
        assert!(!shared.allows_user("ou_alice"));
        let mut unrestricted = read();
        unrestricted["user_id_whitelist"] = serde_json::Value::Null;
        std::fs::write(
            home.join("lark.config"),
            serde_json::to_vec(&unrestricted).unwrap(),
        )
        .unwrap();
        policy.update(&home, &Update::Reload).unwrap();
        assert!(shared.allows_user("ou_anyone"));
        assert!(read()["user_id_whitelist"].is_null());
        assert!(
            policy
                .update(&home, &Update::Remove(vec!["ou_alice".into()]))
                .is_err()
        );
        policy
            .update(&home, &Update::Add(vec!["ou_bob".into()]))
            .unwrap();
        assert!(!shared.allows_user("ou_alice"));
        assert!(shared.allows_user("ou_bob"));

        let mut edited = read();
        edited["user_id_whitelist"] = json!(["ou_carol"]);
        std::fs::write(
            home.join("lark.config"),
            serde_json::to_vec(&edited).unwrap(),
        )
        .unwrap();
        policy.update(&home, &Update::Reload).unwrap();
        assert!(shared.allows_user("ou_carol"));
        assert!(!shared.allows_user("ou_bob"));

        std::fs::remove_file(home.join("lark.config")).unwrap();
        assert!(policy.update(&home, &Update::Reload).is_err());
        assert_eq!(shared.snapshot(), Some(vec!["ou_carol".into()]));
        std::fs::remove_dir_all(home).unwrap();
    }
}
