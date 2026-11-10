use super::{
    sdk::{ApiFailure, MessageResource},
    session::Gateway,
    store::{Record, ThreadState},
};
use serde_json::json;
use std::time::Duration;

const DEDUP_WINDOW_MS: i64 = 3_600_000;

fn ready(gateway: &Gateway, id: &str, message: MessageResource) -> anyhow::Result<Record> {
    let record = gateway.store.get(id);
    message.validate(&record.binding.chat_id, None)?;
    anyhow::ensure!(
        !message.root_id.is_empty(),
        "Thread root metadata unavailable"
    );
    anyhow::ensure!(
        message.root_id == record.message_id || message.root_id == record.root_id,
        "Thread root does not match the input"
    );
    let record = gateway.store.update(id, |r| {
        r.binding.thread_id = message.thread_id.clone();
        r.session_id = message.thread_id;
        r.root_id = message.root_id;
        r.thread_state = ThreadState::Ready;
    })?;
    // Repaired again on every admission if a crash separates these two writes.
    gateway.store.bind_thread(&record, &gateway.config)?;
    Ok(record)
}

pub fn confirm_root_event(gateway: &Gateway, id: &str, event: &Record) -> anyhow::Result<()> {
    let root = gateway.store.get(id);
    anyhow::ensure!(
        root.message_id == event.root_id
            && root.binding.app_id == event.binding.app_id
            && root.binding.tenant_key == event.binding.tenant_key
            && root.binding.chat_id == event.binding.chat_id,
        "Thread event root mismatch"
    );
    if root.thread_state == ThreadState::Ready {
        anyhow::ensure!(
            root.session_id == event.binding.thread_id,
            "Thread root binding conflict"
        );
    } else if root.outcome.is_none() && root.thread_state != ThreadState::Failed {
        // Authenticated receive events provide the same authoritative root and
        // thread identity as message metadata. This reconciles a lost creation
        // response without issuing another reply.
        ready(
            gateway,
            id,
            MessageResource {
                message_id: root.message_id.clone(),
                root_id: root.message_id,
                chat_id: event.binding.chat_id.clone(),
                thread_id: event.binding.thread_id.clone(),
            },
        )?;
    }
    Ok(())
}

pub async fn resolve(gateway: &Gateway, id: &str) -> anyhow::Result<Record> {
    let mut record = gateway.store.get(id);
    if record.session_mapping_version == 1 {
        return Ok(record);
    }
    if record.thread_state == ThreadState::Ready {
        gateway.store.bind_thread(&record, &gateway.config)?;
        return Ok(record);
    }
    anyhow::ensure!(
        record.thread_state != ThreadState::Failed,
        "Thread creation previously failed"
    );
    if let Some(message) = gateway.store.root_thread(&record)? {
        return ready(gateway, id, message);
    }
    // Query before retrying an uncertain request. Never substitute message_id for thread_id.
    if record.thread_create_attempt.is_some() || !record.root_id.is_empty() {
        for anchor in [&record.thread_reply_message_id, &record.message_id] {
            if anchor.is_empty() {
                continue;
            }
            if let Ok(message) = gateway.sdk.message(anchor).await {
                if message.validate(&record.binding.chat_id, None).is_ok() {
                    return ready(gateway, id, message);
                }
            }
        }
    }
    let now = chrono::Utc::now().timestamp_millis();
    if record
        .thread_create_attempt
        .is_some_and(|t| now - t >= DEDUP_WINDOW_MS)
    {
        gateway
            .store
            .update(id, |r| r.thread_state = ThreadState::AcceptanceUnknown)?;
        anyhow::bail!("话题创建接受状态不确定，已超过去重窗口；本次未提交 Agent，请管理员核查");
    }
    let had_attempt = record.thread_create_attempt.is_some();
    record = gateway.store.update(id, |r| {
        r.thread_state = ThreadState::Creating;
        r.thread_create_attempt.get_or_insert(now);
        r.thread_reply_pending_removal = true;
    })?;
    let mut last_error = None;
    let mut uncertain = had_attempt;
    for attempt in 0..3 {
        let result = gateway
            .sdk
            .thread_reply(
                &record.message_id,
                &record.binding.chat_id,
                "post",
                json!({"zh_cn":{"title":"","content":[[{"tag":"emotion","emoji_type":"Typing"}]]}})
                    .to_string(),
                &record.thread_create_uuid,
            )
            .await;
        match result {
            Ok(message) => {
                uncertain = true;
                gateway.store.update(id, |r| {
                    r.thread_reply_message_id = message.message_id.clone()
                })?;
                match ready(gateway, id, message) {
                    Ok(record) => return Ok(record),
                    Err(error) if gateway.store.get(id).thread_state == ThreadState::Ready => {
                        return Err(error.context("话题已创建但绑定保存失败；本次未提交 Agent"));
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            Err(error) => {
                if error.downcast_ref::<ApiFailure>().is_some_and(|e| {
                    !matches!(e.code, 230020 | 429 | 99991400 | 99991401)
                        && !(500..600).contains(&e.code)
                }) {
                    gateway
                        .store
                        .update(id, |r| r.thread_state = ThreadState::Failed)?;
                    return Err(error.context("无法创建话题，本次未执行"));
                }
                if error.downcast_ref::<ApiFailure>().is_none() {
                    uncertain = true;
                }
                last_error = Some(error);
            }
        }
        // Reconcile success whose reply resource was incomplete, or a lost response.
        for anchor in [
            &gateway.store.get(id).thread_reply_message_id,
            &record.message_id,
        ] {
            if anchor.is_empty() {
                continue;
            }
            if let Ok(message) = gateway.sdk.message(anchor).await {
                if message.validate(&record.binding.chat_id, None).is_ok() {
                    return ready(gateway, id, message);
                }
            }
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await;
        }
    }
    gateway.store.update(id, |r| {
        r.thread_state = if uncertain {
            ThreadState::AcceptanceUnknown
        } else {
            ThreadState::Failed
        }
    })?;
    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("Thread creation not confirmed"))
        .context(if uncertain {
            "话题创建未确认，本次未提交 Agent；保留原 UUID 等待恢复"
        } else {
            "无法创建话题，本次未执行；请稍后发送新的普通消息重试"
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        lark::{
            config::Config,
            identity,
            sdk::{Sdk, tests::mock},
            store::Store,
        },
        protocol::SessionBinding,
    };
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{Mutex, Semaphore, watch};

    fn gateway(base: String) -> Arc<Gateway> {
        let home = std::env::temp_dir().join(format!("fae-thread-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        let config: Config = serde_json::from_value(json!({
            "version":1,"app_id":"app","app_secret":"secret","tenant_key":"tenant",
            "agent_id":"fae","workspace":home
        }))
        .unwrap();
        let (_, stopping) = watch::channel(false);
        Arc::new(Gateway {
            home: home.clone(),
            config: config.clone(),
            store: Arc::new(Store::open(&home, 7).unwrap()),
            sdk: Sdk::for_test(config, base),
            stopping,
            slots: Arc::new(Semaphore::new(1)),
            sessions: Mutex::new(HashMap::new()),
            tasks: std::sync::Mutex::new(tokio::task::JoinSet::new()),
        })
    }
    fn input(gateway: &Gateway, message: &str, thread: &str, root: &str) -> String {
        let binding: SessionBinding = serde_json::from_value(json!({
            "session_mapping_version":2,"thread_id":thread,"channel":"lark","user_id":"ou_a",
            "app_id":"app","tenant_key":"tenant","chat_type":"group","chat_id":"chat"
        }))
        .unwrap();
        let id = identity::receipt_id("app", "tenant", message);
        let mut record = Record::new(
            id.clone(),
            thread.into(),
            binding,
            message.into(),
            "hello".into(),
            None,
        );
        record.root_id = root.into();
        gateway.store.insert(record).unwrap();
        id
    }
    fn resource(message: &str, thread: &str, root: &str) -> serde_json::Value {
        json!({"message_id":message,"chat_id":"chat","thread_id":thread,"root_id":root})
    }

    #[tokio::test]
    async fn typing_thread_reply_is_withdrawn_after_output_and_cleanup_survives_restart() {
        let mut withdrawals = 0;
        let (base, requests, task) = mock(move |path, body| {
            if path.ends_with("om_root/reply") {
                if body["msg_type"] == "post" {
                    let content: serde_json::Value =
                        serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
                    assert_eq!(
                        content["zh_cn"]["content"],
                        json!([[{"tag":"emotion","emoji_type":"Typing"}]])
                    );
                    json!({"code":0,"data":resource("om_typing", "omt_T1", "om_root")})
                } else {
                    assert_eq!(body["content"], json!({"text":"正式回复"}).to_string());
                    json!({"code":0,"data":resource("om_final", "omt_T1", "om_root")})
                }
            } else {
                assert!(path.ends_with("/messages/om_typing"));
                withdrawals += 1;
                if withdrawals == 1 {
                    json!({"code":99991400,"msg":"rate limited"})
                } else {
                    json!({"code":0})
                }
            }
        })
        .await;
        let gateway = gateway(base);
        let id = input(&gateway, "om_root", "", "");
        let record = resolve(&gateway, &id).await.unwrap();
        assert!(record.thread_reply_pending_removal);
        assert_eq!(requests.lock().unwrap().len(), 2); // Token + thread creation.
        gateway
            .store
            .update(&id, |r| {
                r.outcome = Some("succeeded".into());
                r.notice = Some("正式回复".into());
            })
            .unwrap();
        let writer = super::super::card::Writer {
            sdk: gateway.sdk.clone(),
            store: gateway.store.clone(),
        };
        assert!(writer.flush(&id).await.is_err());
        let record = gateway.store.get(&id);
        assert!(record.notice_sent && record.thread_reply_pending_removal);
        assert!(record.state != super::super::store::State::Delivered);
        let writer = super::super::card::Writer {
            sdk: gateway.sdk.clone(),
            store: Arc::new(Store::open(&gateway.home, 7).unwrap()),
        };
        writer.flush(&id).await.unwrap();
        let record = writer.store.get(&id);
        assert!(!record.thread_reply_pending_removal);
        assert!(record.state == super::super::store::State::Delivered);
        let seen = requests.lock().unwrap();
        let replies: Vec<_> = seen.iter().filter(|(p, _)| p.ends_with("/reply")).collect();
        assert_eq!(replies.len(), 2); // Cleanup retry never sends the final output again.
        assert!(
            seen.iter()
                .position(|(p, b)| p.ends_with("/reply") && b["msg_type"] == "text")
                < seen
                    .iter()
                    .position(|(p, _)| p.ends_with("/messages/om_typing"))
        );
        drop(seen);
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test]
    async fn outside_messages_create_distinct_threads_and_bindings_outlive_receipts() {
        let (base, requests, task) = mock(|path, body| {
            if !path.ends_with("/reply") {
                let message = path.rsplit('/').next().unwrap();
                return json!({"code":0,"data":{"items":[resource(message, "", "")]}});
            }
            assert_eq!(body["reply_in_thread"], true);
            let root = path.split('/').nth(5).unwrap();
            json!({"code":0,"data":resource(&format!("om_accept_{root}"), &format!("omt_{root}"), root)})
        }).await;
        let gateway = gateway(base);
        let first = input(&gateway, "om_one", "", "");
        let second = input(&gateway, "om_two", "", "");
        let t1 = resolve(&gateway, &first).await.unwrap();
        let t2 = resolve(&gateway, &second).await.unwrap();
        assert_ne!(t1.session_id, t2.session_id);
        assert_eq!(t1.session_id, t1.binding.thread_id);
        assert_eq!(
            resolve(&gateway, &first).await.unwrap().session_id,
            t1.session_id
        );
        let followup = input(&gateway, "om_followup", &t1.session_id, "om_one");
        assert_eq!(
            resolve(&gateway, &followup).await.unwrap().session_id,
            t1.session_id
        );
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.ends_with("/reply"))
                .count(),
            2
        );
        // A normal reference must not inherit an earlier thread.
        let quoted = input(&gateway, "om_quote", "", "om_one");
        assert!(
            gateway
                .store
                .predecessors(&gateway.store.get(&quoted))
                .is_empty()
        );
        let t3 = resolve(&gateway, &quoted).await.unwrap();
        assert_ne!(t1.session_id, t3.session_id);
        let binding = gateway.store.bind_thread(&t1, &gateway.config).unwrap();
        gateway
            .store
            .update(&first, |r| {
                r.state = super::super::store::State::Delivered;
                r.delivered_at = Some(chrono::Utc::now().timestamp_millis() - 8 * 86_400_000);
            })
            .unwrap();
        let reopened = Store::open(&gateway.home, 7).unwrap();
        let saved = reopened
            .bind_thread(&reopened.get(&followup), &gateway.config)
            .unwrap();
        assert_eq!(saved.anchor, binding.anchor);
        assert_eq!(saved.binding, t1.binding);
        assert_eq!(reopened.get(&second).session_id, t2.session_id);
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test]
    async fn frequency_limit_retries_same_creation_uuid_without_chat_fallback() {
        let mut attempts = 0;
        let (base, requests, task) = mock(move |path, _| {
            if path.ends_with("/reply") {
                attempts += 1;
                if attempts == 1 {
                    json!({"code":230020,"msg":"frequency limit"})
                } else {
                    json!({"code":0,"data":resource("om_accept", "omt_T1", "om_root")})
                }
            } else {
                json!({"code":0,"data":{"items":[resource("om_root", "", "")]}})
            }
        })
        .await;
        let gateway = gateway(base);
        let id = input(&gateway, "om_root", "", "");
        assert_eq!(resolve(&gateway, &id).await.unwrap().session_id, "omt_T1");
        let seen = requests.lock().unwrap();
        let replies: Vec<_> = seen.iter().filter(|(p, _)| p.ends_with("/reply")).collect();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0].1["uuid"], replies[1].1["uuid"]);
        assert!(
            seen.iter()
                .all(|(p, _)| p != "/open-apis/im/v1/messages?receive_id_type=chat_id")
        );
        drop(seen);
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test]
    async fn missing_thread_never_submits_and_expired_creation_never_reissues() {
        let (base, requests, task) = mock(|path, _| {
            if path.ends_with("/reply") {
                json!({"code":0,"data":{"message_id":"om_accept"}})
            } else {
                let id = path.rsplit('/').next().unwrap();
                json!({"code":0,"data":{"items":[resource(id, "", "")]}})
            }
        })
        .await;
        let gateway = gateway(base);
        let id = input(&gateway, "om_root", "", "");
        gateway.recover(id.clone()).await;
        let record = gateway.store.get(&id);
        assert_eq!(record.thread_state, ThreadState::AcceptanceUnknown);
        assert!(
            record.session_id.is_empty() && record.stream_id.is_none() && record.outcome.is_none()
        );
        assert!(gateway.sessions.lock().await.is_empty());
        let attempts = requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.ends_with("/reply"))
            .count();
        gateway
            .store
            .update(&id, |r| {
                r.thread_create_attempt =
                    Some(chrono::Utc::now().timestamp_millis() - DEDUP_WINDOW_MS - 1)
            })
            .unwrap();
        assert!(resolve(&gateway, &id).await.is_err());
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.ends_with("/reply"))
                .count(),
            attempts
        );
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test]
    async fn restart_reconciles_creation_before_replying_again() {
        let (base, requests, task) = mock(|path, _| {
            assert!(!path.ends_with("/reply"));
            json!({"code":0,"data":{"items":[resource("om_root", "omt_existing", "om_root")]}})
        })
        .await;
        let gateway = gateway(base);
        let id = input(&gateway, "om_root", "", "");
        gateway
            .store
            .update(&id, |r| {
                r.thread_state = ThreadState::Creating;
                r.thread_create_attempt = Some(chrono::Utc::now().timestamp_millis());
            })
            .unwrap();
        assert_eq!(
            resolve(&gateway, &id).await.unwrap().session_id,
            "omt_existing"
        );
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .all(|(p, _)| !p.ends_with("/reply"))
        );
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test]
    async fn unsupported_threads_fail_before_execution() {
        let (base, requests, task) = mock(|path, _| {
            assert!(path.ends_with("/reply"));
            json!({"code":230071,"msg":"chat does not support threads"})
        })
        .await;
        let gateway = gateway(base);
        let id = input(&gateway, "om_root", "", "");
        gateway.recover(id.clone()).await;
        let record = gateway.store.get(&id);
        assert_eq!(record.thread_state, ThreadState::Failed);
        assert_eq!(record.outcome.as_deref(), Some("failed"));
        assert!(record.stream_id.is_none());
        assert!(record.notice.unwrap().contains("本次未执行"));
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.ends_with("/reply"))
                .count(),
            1
        );
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test]
    async fn withdrawn_output_anchor_only_uses_verified_same_thread_anchor() {
        let (base, requests, task) = mock(|path, _| {
            if path.ends_with("om_withdrawn/reply") {
                json!({"code":230011,"msg":"withdrawn"})
            } else if path.ends_with("/reply") {
                json!({"code":0,"data":resource("om_output", "omt_T1", "om_root")})
            } else {
                json!({"code":0,"data":{"items":[resource("om_accept", "omt_T1", "om_root")]}})
            }
        })
        .await;
        let gateway = gateway(base);
        let id = input(&gateway, "om_withdrawn", "omt_T1", "om_root");
        gateway
            .store
            .update(&id, |r| r.thread_reply_message_id = "om_accept".into())
            .unwrap();
        gateway
            .store
            .bind_thread(&gateway.store.get(&id), &gateway.config)
            .unwrap();
        gateway
            .sdk
            .text_record(&gateway.store, &id, "result", "stable-uuid")
            .await
            .unwrap();
        assert_eq!(gateway.store.get(&id).reply_anchor_message_id, "om_accept");
        let seen = requests.lock().unwrap();
        let replies: Vec<_> = seen.iter().filter(|(p, _)| p.ends_with("/reply")).collect();
        assert_eq!(replies.len(), 2);
        assert!(
            replies
                .iter()
                .all(|(_, b)| b["reply_in_thread"] == true && b["uuid"] == "stable-uuid")
        );
        assert!(replies[1].0.ends_with("om_accept/reply"));
        drop(seen);
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }

    #[tokio::test]
    async fn wrong_output_thread_is_not_marked_delivered() {
        let (base, _, task) =
            mock(|_, _| json!({"code":0,"data":resource("om_output", "omt_wrong", "om_root")}))
                .await;
        let gateway = gateway(base);
        let id = input(&gateway, "om_input", "omt_T1", "om_root");
        assert!(
            gateway
                .sdk
                .text_record(&gateway.store, &id, "result", "uuid")
                .await
                .is_err()
        );
        assert!(!gateway.store.get(&id).notice_sent);
        task.abort();
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }
    #[tokio::test]
    async fn root_admission_precedes_early_followup_without_cyclic_waits() {
        let gateway = gateway("http://127.0.0.1:1".into());
        let followup = input(&gateway, "om_followup", "omt_T1", "om_root");
        let root = input(&gateway, "om_root", "", "");
        let earlier = gateway.store.predecessors(&gateway.store.get(&followup));
        assert_eq!(earlier.len(), 1);
        assert_eq!(earlier[0].id, root);
        confirm_root_event(&gateway, &root, &gateway.store.get(&followup)).unwrap();
        assert_eq!(gateway.store.get(&root).session_id, "omt_T1");
        assert!(
            gateway
                .store
                .predecessors(&gateway.store.get(&root))
                .is_empty()
        );
        assert_eq!(
            gateway.store.predecessors(&gateway.store.get(&followup))[0].id,
            root
        );
        assert!(
            gateway
                .store
                .update(&root, |r| {
                    r.binding.thread_id = "omt_other".into();
                    r.session_id = "omt_other".into();
                })
                .is_err()
        );
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }
    #[tokio::test]
    async fn outside_management_commands_do_not_create_threads_or_open_agents() {
        let gateway = gateway("http://127.0.0.1:1".into());
        for (index, command) in [
            "/help",
            "/status",
            "/clean",
            "/steer hello",
            "/session new",
            "/session id=omt_other",
        ]
        .into_iter()
        .enumerate()
        {
            let id = input(&gateway, &format!("om_command_{index}"), "", "");
            gateway
                .store
                .update(&id, |r| r.text = command.into())
                .unwrap();
            gateway.recover(id.clone()).await;
            let record = gateway.store.get(&id);
            assert_eq!(record.outcome.as_deref(), Some("succeeded"));
            assert!(record.notice.is_some());
            assert!(
                record.session_id.is_empty()
                    && record.thread_create_attempt.is_none()
                    && record.stream_id.is_none()
            );
        }
        assert!(gateway.sessions.lock().await.is_empty());
        std::fs::remove_dir_all(&gateway.home).unwrap();
    }
}
