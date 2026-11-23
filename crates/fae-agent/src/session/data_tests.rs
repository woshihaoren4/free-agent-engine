use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::*;
use crate::{
    ContextData, MemoryView, SessionMessage, UserInfo, UserMemory,
    protocol::{Content, ContentBlock, ErrorInfo},
};

fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let encoded = serde_json::to_vec(value).unwrap();
    assert_eq!(&serde_json::from_slice::<T>(&encoded).unwrap(), value);
}

fn session_fixture() -> Value {
    json!({
        "context": {
            "user_info": {"user_id": "user-1", "name": "Alice"},
            "session_config": {
                "session_id": "session-1",
                "name": "Implementation",
                "lifecycle": "open",
                "created_at": "2026-11-23T01:00:00Z",
                "updated_at": "2026-11-23T01:02:00Z",
                "target": {"type": "agent", "agent_id": "developer"},
                "workspace": {
                    "root": "/workspace/project",
                    "project_files": [
                        {"path": ".fae/workspace.md", "content": "# Workspace"},
                        {"path": ".fae/rules.md", "content": "# Rules"},
                        {"path": ".fae/agents.md", "content": null},
                        {"path": ".fae/project.md", "content": "# Project"}
                    ]
                },
                "options": {"mode": "todo"},
                "channels": [
                    {"type": "tui", "client_id": "terminal-1"},
                    {"type": "lark", "app_id": "app-1", "chat_id": "chat-1", "thread_id": "thread-1"},
                    {"type": "sdk", "client_id": "sdk-1"}
                ]
            },
            "memory_view": {
                "history": [
                    {"role": "user", "content": "Implement this", "images": ["image.png"]},
                    {"role": "assistant", "content": "Done"},
                    {"role": "summary", "content": "Earlier conversation"}
                ],
                "history_offset": 12,
                "summary": "Earlier conversation",
                "user_memories": [{
                    "id": 7,
                    "category": "preference",
                    "content": "Use Rust",
                    "confidence": "user_stated",
                    "updated_at": "2026-09-24 14:08:56"
                }]
            },
            "latest_execution_view": {
                "execution_id": "execution-1",
                "target": {"type": "workflow", "workflow_id": "build"},
                "effective": {
                    "target": {"type": "workflow", "workflow_id": "build"},
                    "workspace": "/workspace/project",
                    "options": {},
                    "definitions": [{
                        "target": {"type": "workflow", "workflow_id": "build"},
                        "source": "/definitions/build.yaml",
                        "content_fingerprint": "sha256:abc"
                    }]
                },
                "status": "succeeded",
                "invocations": [
                    {
                        "invocation_id": "invocation-root",
                        "parent_invocation_id": null,
                        "kind": "plan",
                        "plan_id": "plan-1",
                        "task_id": null,
                        "runtime_id": null,
                        "agent_id": null,
                        "workflow_id": "build",
                        "node_id": null,
                        "model": null,
                        "status": "succeeded"
                    },
                    {
                        "invocation_id": "invocation-child",
                        "parent_invocation_id": "invocation-root",
                        "kind": "runtime",
                        "plan_id": "plan-1",
                        "task_id": "task-1",
                        "runtime_id": "model",
                        "agent_id": "developer",
                        "workflow_id": "build",
                        "node_id": "generate",
                        "model": {"provider": "provider-1", "model": "model-1"},
                        "status": "succeeded"
                    }
                ],
                "started_at": "2026-11-23T01:01:00Z",
                "ended_at": "2026-11-23T01:02:00Z",
                "outcome": {
                    "type": "succeeded",
                    "result": [
                        {"type": "text", "text": "Done"},
                        {"type": "resource", "resource": {
                            "uri": "file:///workspace/project/result.png",
                            "media_type": "image/png",
                            "name": "result.png"
                        }},
                        {"type": "json", "value": {"count": 2, "items": [true, null]}}
                    ]
                },
                "inputs": [
                    {
                        "request_id": "input-1",
                        "execution_id": "execution-1",
                        "status": "applied",
                        "reason": null,
                        "received_at": "2026-11-23T01:01:00Z",
                        "processed_at": "2026-11-23T01:01:01Z"
                    },
                    {
                        "request_id": "input-2",
                        "execution_id": "execution-1",
                        "status": "not_applied",
                        "reason": "execution_finished",
                        "received_at": "2026-11-23T01:01:59Z",
                        "processed_at": "2026-11-23T01:02:00Z"
                    }
                ]
            }
        }
    })
}

#[test]
fn complete_session_roundtrip_preserves_nested_business_data() {
    let fixture = session_fixture();
    let session: Session = serde_json::from_value(fixture.clone()).unwrap();
    roundtrip(&session);
    roundtrip(&session.context);
    assert_eq!(serde_json::to_value(&session).unwrap(), fixture);
    let execution = session.context.latest_execution_view.as_ref().unwrap();
    assert_eq!(
        execution.invocations[1].parent_invocation_id.as_ref(),
        Some(&execution.invocations[0].invocation_id)
    );
    assert_eq!(session.context.memory_view.history_offset, 12);
    assert_eq!(
        session.context.memory_view.user_memories[0].updated_at,
        "2026-09-24 14:08:56"
    );
}

#[test]
fn existing_history_and_memory_schema_are_not_wrapped_or_rewritten() {
    let fixture = session_fixture();
    let session: Session = serde_json::from_value(fixture.clone()).unwrap();
    let view = &fixture["context"]["memory_view"];
    for (message, encoded) in session
        .context
        .memory_view
        .history
        .iter()
        .zip(view["history"].as_array().unwrap())
    {
        let original: SessionMessage = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(message, &original);
        assert_eq!(serde_json::to_value(message).unwrap(), *encoded);
    }
    let memory: UserMemory = serde_json::from_value(view["user_memories"][0].clone()).unwrap();
    assert_eq!(session.context.memory_view.user_memories[0], memory);
    assert_eq!(
        serde_json::to_value(memory).unwrap(),
        view["user_memories"][0]
    );
}

#[test]
fn context_without_execution_or_loaded_data_roundtrips() {
    let mut fixture = session_fixture();
    fixture["context"]["latest_execution_view"] = Value::Null;
    fixture["context"]["memory_view"] = serde_json::to_value(MemoryView::default()).unwrap();
    let session: Session = serde_json::from_value(fixture).unwrap();
    assert!(session.context.latest_execution_view.is_none());
    roundtrip(&session);
    // An empty view is data only; it does not assert that storage has already been loaded.
    roundtrip(&ContextData {
        user_info: UserInfo {
            user_id: UserId::new("user-1").unwrap(),
            name: None,
        },
        ..session.context
    });
}

#[test]
fn identifiers_validate_construction_and_json_for_every_identity_type() {
    macro_rules! check_id {
        ($($ty:ty),+ $(,)?) => {$(
            let id = <$ty>::new("id-1").unwrap();
            roundtrip(&id);
            assert_eq!(serde_json::to_value(&id).unwrap(), json!("id-1"));
            assert_eq!(id.as_str(), "id-1");
            assert_eq!(id.to_string(), "id-1");
            assert_eq!(id.clone().into_string(), "id-1");
            assert_eq!("id-1".parse::<$ty>().unwrap(), id);
            for invalid in ["", " ", " padded", "padded ", "new\nline", "null\0byte"] {
                assert!(<$ty>::new(invalid).is_err(), "{invalid:?}");
                assert!(serde_json::from_value::<$ty>(json!(invalid)).is_err());
            }
            assert!(<$ty>::new("a".repeat(MAX_ID_BYTES)).is_ok());
            assert!(<$ty>::new("a".repeat(MAX_ID_BYTES + 1)).is_err());
            assert!(serde_json::from_value::<$ty>(json!("a".repeat(MAX_ID_BYTES + 1))).is_err());
            assert!(serde_json::from_value::<$ty>(json!(12)).is_err());
            assert!(serde_json::from_value::<$ty>(Value::Null).is_err());
        )+};
    }
    check_id!(
        UserId,
        SessionId,
        ExecutionId,
        InvocationId,
        PlanInstanceId,
        TaskId,
        RequestId
    );
    assert!(UserId::new("中".repeat(MAX_ID_BYTES / 3 + 1)).is_err());
    // IDs remain opaque identities, rather than requiring an executable route or a SHA256 digest.
    assert!(RequestId::new("lark:message-1").is_ok());
}

#[test]
fn timestamps_are_validated_and_normalized_to_utc() {
    let timestamp = Timestamp::new("2026-11-23T09:02:03.123+08:00").unwrap();
    assert_eq!(timestamp.as_str(), "2026-11-23T01:02:03.123Z");
    roundtrip(&timestamp);
    assert_eq!(
        serde_json::from_value::<Timestamp>(json!("2026-11-23T01:02:03.123+00:00")).unwrap(),
        timestamp
    );
    for invalid in [
        "",
        "2026-11-23",
        "2026-02-30T00:00:00Z",
        "2026-11-23T00:00:00",
        "not-a-date",
    ] {
        assert!(Timestamp::new(invalid).is_err(), "{invalid:?}");
        assert!(serde_json::from_value::<Timestamp>(json!(invalid)).is_err());
    }
    assert!(serde_json::from_value::<Timestamp>(json!(123)).is_err());
}

#[test]
fn utc_normalization_cannot_create_an_unreadable_timestamp() {
    for invalid in ["9999-12-31T23:59:59-01:00", "0000-01-01T00:00:00+01:00"] {
        assert!(Timestamp::new(invalid).is_err(), "{invalid}");
        assert!(serde_json::from_value::<Timestamp>(json!(invalid)).is_err());
    }
    let extended_year = chrono::DateTime::from_timestamp(253402300800, 0).unwrap();
    assert!(Timestamp::from_utc(extended_year).is_err());
    roundtrip(&Timestamp::new("9999-12-31T23:59:59Z").unwrap());
    roundtrip(&Timestamp::new("0000-01-01T00:00:00Z").unwrap());
}

#[test]
fn public_outcomes_use_registered_content_and_can_be_read_repeatedly() {
    let session: Session = serde_json::from_value(session_fixture()).unwrap();
    let execution = session.context.latest_execution_view.unwrap();
    let before = serde_json::to_value(&execution).unwrap();
    for _ in 0..2 {
        let outcome = execution.outcome.as_ref().unwrap();
        roundtrip(outcome);
        assert!(matches!(outcome, ExecutionOutcome::Succeeded { result } if result.len() == 3));
    }
    assert_eq!(serde_json::to_value(&execution).unwrap(), before);
    let failure = ExecutionOutcome::Failed {
        error: ErrorInfo {
            code: "save_failed".into(),
            message: "History write failed".into(),
        },
    };
    assert_eq!(
        serde_json::to_value(&failure).unwrap(),
        json!({
            "type": "failed", "error": {"code": "save_failed", "message": "History write failed"}
        })
    );
    roundtrip(&failure);
    roundtrip(&ExecutionOutcome::Cancelled {
        reason: "User stopped".into(),
    });
    roundtrip(&Vec::<ContentBlock>::new());
}

#[test]
fn dto_boundary_rejects_unsupported_content_and_non_allowlisted_options() {
    for invalid in [
        json!([{"type": "native", "handle": "123"}]),
        json!([{"type": "unsupported_output", "debug": "value"}]),
        json!([{"type": "text", "text": "Done", "handle": "123"}]),
        json!([{"type": "text", "text": 12}]),
        json!([{"type": "resource", "resource": {"uri": "image.png"}}]),
    ] {
        assert!(serde_json::from_value::<Content>(invalid).is_err());
    }
    for invalid in [
        json!({"mode": "unknown"}),
        json!({"model": "other"}),
        json!({"temperature": 0.5}),
        json!({"user_id": "forged"}),
        json!({"options": {}}),
    ] {
        assert!(serde_json::from_value::<ExecutionOptions>(invalid).is_err());
    }
    roundtrip(&ExecutionOptions::default());
    roundtrip(&ExecutionOptions {
        mode: Some(AgentMode::Todo),
    });
    let mut fixture = session_fixture();
    fixture["context"]["cancel_handle"] = json!("123");
    assert!(serde_json::from_value::<Session>(fixture).is_err());
}

#[test]
fn execution_and_invocation_statuses_keep_cleanup_nonterminal() {
    for status in [
        ExecutionStatus::Preparing,
        ExecutionStatus::Running,
        ExecutionStatus::Waiting,
        ExecutionStatus::Stopping,
        ExecutionStatus::Finishing,
        ExecutionStatus::Succeeded,
        ExecutionStatus::Failed,
        ExecutionStatus::Cancelled,
    ] {
        roundtrip(&status);
        assert_eq!(
            status.is_terminal(),
            matches!(
                status,
                ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
            )
        );
    }
    for status in [
        InvocationStatus::Preparing,
        InvocationStatus::Running,
        InvocationStatus::Waiting,
        InvocationStatus::Stopping,
        InvocationStatus::Succeeded,
        InvocationStatus::Failed,
        InvocationStatus::Cancelled,
    ] {
        roundtrip(&status);
        assert_eq!(
            status.is_terminal(),
            matches!(
                status,
                InvocationStatus::Succeeded
                    | InvocationStatus::Failed
                    | InvocationStatus::Cancelled
            )
        );
    }
    assert!(!ExecutionStatus::Stopping.is_terminal());
    assert!(!ExecutionStatus::Finishing.is_terminal());
    for status in [
        InputStatus::Queued,
        InputStatus::Applied,
        InputStatus::NotApplied,
    ] {
        roundtrip(&status);
    }
    roundtrip(&SessionLifecycle::Archived);
    roundtrip(&ChannelBinding::Lark {
        app_id: "app-1".into(),
        chat_id: "chat-1".into(),
        thread_id: None,
    });
}
