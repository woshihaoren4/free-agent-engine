use super::config::Config;
use crate::protocol::{OpenSession, SessionBinding, SessionTarget};
use sha2::{Digest, Sha256};

pub fn hash(tuple: &[&str]) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(tuple).expect("string tuple"))
    )
}
pub fn session_id(binding: &SessionBinding) -> String {
    if binding.session_mapping_version == 2 {
        return binding.thread_id.clone();
    }
    format!(
        "lark-v1-{}",
        hash(&[
            "fae-lark-session-v1",
            &binding.app_id,
            &binding.tenant_key,
            &binding.user_id,
            &binding.chat_type,
            &binding.chat_id
        ])
    )
}
pub fn receipt_id(app: &str, tenant: &str, message: &str) -> String {
    hash(&["fae-lark-message-v1", app, tenant, message])
}
pub fn owns_session(binding: &SessionBinding, id: &str) -> bool {
    if binding.session_mapping_version == 2 {
        return valid_thread(id) && id == binding.thread_id;
    }
    if binding.session_mapping_version != 1 || !binding.thread_id.is_empty() {
        return false;
    }
    let base = session_id(binding);
    id == base || id.strip_prefix(&format!("{base}--")).is_some_and(|suffix| {
        suffix.len() == 36 && suffix.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() }
        })
    })
}
pub fn valid_thread(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256
        && id != "." && id != ".."
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
pub fn worker_key(binding: &SessionBinding, agent: &str) -> String {
    hash(&["fae-lark-worker-v2", agent, &binding.app_id, &binding.tenant_key,
        &binding.user_id, &binding.chat_type, &binding.chat_id, &session_id(binding)])
}
pub fn validate(config: &Config, open: &OpenSession) -> anyhow::Result<()> {
    let binding = open
        .binding
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Lark binding required"))?;
    anyhow::ensure!(
        binding.channel == "lark"
            && binding.app_id == config.app_id
            && binding.tenant_key == config.tenant_key,
        "Lark application or tenant mismatch"
    );
    anyhow::ensure!(
        !binding.user_id.is_empty()
            && !binding.chat_id.is_empty()
            && matches!(binding.chat_type.as_str(), "p2p" | "group"),
        "Invalid Lark identity"
    );
    anyhow::ensure!(
        open.session_id.as_deref().is_some_and(|id| owns_session(binding, id)),
        "Lark session identity mismatch"
    );
    anyhow::ensure!(binding.session_mapping_version == 2 || open.resume.is_some(),
        "Legacy Lark mapping is only available for recovery");
    anyhow::ensure!(
        open.workspace == config.workspace,
        "Lark workspace mismatch"
    );
    anyhow::ensure!(
        matches!(&open.target, SessionTarget::Agent { source: fae_agent::SingleAgentSource::AgentId(id) } if id == &config.agent_id),
        "Lark Agent mismatch"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_are_stable_and_separate_chats_and_users() {
        let mut b = SessionBinding {
            session_mapping_version: 1,
            thread_id: String::new(),
            channel: "lark".into(),
            app_id: "app".into(),
            tenant_key: "tenant".into(),
            user_id: "u".into(),
            chat_type: "p2p".into(),
            chat_id: "p".into(),
        };
        let original = session_id(&b);
        assert_eq!(original, session_id(&b));
        b.chat_type = "group".into();
        b.chat_id = "g".into();
        assert_ne!(original, session_id(&b));
        let group = session_id(&b);
        b.user_id = "v".into();
        assert_ne!(group, session_id(&b));
    }
    #[test]
    fn thread_mapping_uses_real_id_and_separate_user_workers() {
        let mut binding: SessionBinding = serde_json::from_value(serde_json::json!({
            "session_mapping_version":2,"thread_id":"omt_T1","channel":"lark",
            "app_id":"app","tenant_key":"tenant","user_id":"ou_a","chat_type":"group","chat_id":"chat"
        })).unwrap();
        assert_eq!(session_id(&binding), "omt_T1");
        assert!(owns_session(&binding, "omt_T1"));
        for illegal in ["", ".", "..", "../omt_T1", "omt_T1/child", "omt_T1--other"] {
            assert!(!owns_session(&binding, illegal));
        }
        let worker = worker_key(&binding, "fae");
        binding.user_id = "ou_b".into();
        assert_eq!(session_id(&binding), "omt_T1");
        assert_ne!(worker, worker_key(&binding, "fae"));
        binding.thread_id = "../escape".into();
        assert!(!owns_session(&binding, "../escape"));
    }
}
