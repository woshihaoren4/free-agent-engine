use serde_json::{Value, json};

use super::*;
use crate::{ExecutionId, RequestId, SessionId};

fn request(path: &str, payload: Value) -> Value {
    let mut value = json!({
        "version": 4, "request_id": "request-1", "path": path, "payload": payload
    });
    if path.starts_with("/request/session/") && path != "/request/session/create" {
        value["session_id"] = json!("session-1");
    }
    value
}

fn summary() -> Value {
    json!({
        "user_info": {"user_id": "user-1", "name": null},
        "session_config": {
            "session_id": "session-1", "name": "Session", "lifecycle": "open",
            "created_at": "2026-11-23T01:00:00Z", "updated_at": "2026-11-23T01:00:00Z",
            "target": {"type":"agent", "agent_id":"developer"},
            "workspace": {"root":"/workspace", "project_files":[]},
            "options":{}, "channels":[]
        },
        "context": {
            "memory": {
                "history_loaded":false, "history_offset":0, "history_count":0,
                "user_memories_loaded":false, "user_memory_count":0, "project_files_loaded":false
            },
            "latest_execution":null
        },
        "active_execution":null
    })
}

fn request_examples() -> Vec<(RequestPath, Value)> {
    vec![
        (RequestPath::EngineHello, json!({"versions":[4]})),
        (RequestPath::EngineStatus, json!({})),
        (
            RequestPath::EngineInitialize,
            json!({"agent_id":"developer", "model":"provider/model", "force":false}),
        ),
        (RequestPath::EngineShutdown, json!({})),
        (RequestPath::EnginePluginShow, json!({"id":"*"})),
        (
            RequestPath::EnginePluginInstall,
            json!({"source":"/plugins/example.py"}),
        ),
        (RequestPath::EnginePluginUninstall, json!({"id":"example"})),
        (
            RequestPath::SessionCreate,
            json!({
                "name":"Session", "target":{"type":"agent", "agent_id":"developer"},
                "options":{}, "channels":[]
            }),
        ),
        (RequestPath::SessionGet, json!({})),
        (
            RequestPath::SessionConfigUpdate,
            json!({"workspace":"/workspace"}),
        ),
        (
            RequestPath::SessionConversationList,
            json!({"offset":0, "limit":20}),
        ),
        (RequestPath::SessionConversationClear, json!({})),
        (
            RequestPath::SessionExecutionStart,
            json!({"input":[{"type":"text", "text":"Implement login"}]}),
        ),
        (
            RequestPath::SessionExecutionSteer,
            json!({"execution_id":"execution-1", "input":[{"type":"text", "text":"Use Rust"}]}),
        ),
        (
            RequestPath::SessionExecutionStop,
            json!({"execution_id":"execution-1"}),
        ),
        (RequestPath::SessionExecutionGet, json!({})),
        (
            RequestPath::SessionSubscribe,
            json!({"stream_id":"stream-1", "after_seq":12}),
        ),
        (
            RequestPath::SessionUnsubscribe,
            json!({"subscription_id":"subscription-1"}),
        ),
        (RequestPath::SessionArchive, json!({})),
        (
            RequestPath::PluginWorkerHello,
            json!({"api_version":"0.2.0","instances":["demo:0"]}),
        ),
        (
            RequestPath::PluginInspect,
            json!({"id":"demo","source":"/plugins/demo.py"}),
        ),
        (
            RequestPath::PluginInvoke,
            json!({"point":"SingleAgentHook","method":"on_prompt","location":null,
            "contexts":{"demo:0":context_reference()},"argument":"prompt","group":"group","timeout_ms":1000,"model_members":null}),
        ),
        (
            RequestPath::PluginNext,
            json!({"access":access(),"point":"SingleAgentHook","method":"on_prompt","argument":"prompt"}),
        ),
        (RequestPath::PluginContextGet, access()),
        (RequestPath::PluginContextExport, access()),
        (
            RequestPath::PluginContextStacks,
            json!({"access":access(),"query":{
                "tree":false,"task_id":null,"plan_id":null,"executor":null,"phase":null,"after_sequence":null,"limit":null
            }}),
        ),
        (
            RequestPath::PluginContextFork,
            json!({"access":access(),"spec":{"operation":"child"}}),
        ),
        (
            RequestPath::PluginContextCancel,
            json!({"access":access(),"reason":null}),
        ),
        (
            RequestPath::PluginContextCompleteQuery,
            json!({"access":access(),"text":"result"}),
        ),
        (
            RequestPath::PluginOutputSend,
            json!({"access":access(),"content":[{"type":"text","text":"output"}]}),
        ),
        (
            RequestPath::PluginStreamNext,
            json!({"call_id":"call-1","stream_id":"stream-1","access":null}),
        ),
        (
            RequestPath::PluginStreamClose,
            json!({"call_id":"call-1","stream_id":"stream-1","access":null}),
        ),
        (RequestPath::PluginCallRelease, json!({"call_id":"call-1"})),
        (RequestPath::PluginHostEngine, access()),
        (RequestPath::PluginHostSession, access()),
    ]
}

fn context_reference() -> Value {
    json!({"binding":{"handle":"demo:0","execution_id":null,"invocation_id":null},"session_id":null,"summary":null})
}

fn access() -> Value {
    json!({"call_id":"call-1","member":"demo:0","binding":context_reference()["binding"]})
}

fn response_example(path: RequestPath) -> Value {
    match path {
        RequestPath::EngineHello => json!({
            "version":4,
            "instance":{
                "id":"instance-1", "pid":12, "app_version":"0.1.0",
                "started_at":"2026-11-23T01:00:00Z", "address":"127.0.0.1", "port":6666,
                "executable":"/bin/fae", "home":"/home/fae"
            },
            "capabilities":{
                "max_history_limit":100,
                "replay":{"max_events":4096, "max_bytes":16777216, "persistent":false},
                "requests":{"max_entries":256, "user_scoped":false, "persistent":false},
                "execution_restore":false, "full_config_restore":false
            }
        }),
        RequestPath::EngineStatus => {
            json!({"sessions":2, "active_executions":1, "shutting_down":false})
        }
        RequestPath::EngineInitialize => {
            json!({"config_path":"/agents/developer_config.json", "skill_count":2})
        }
        RequestPath::EngineShutdown => json!({"accepted":true}),
        RequestPath::EnginePluginShow => json!({"plugins":[{
            "id":"example", "order":1, "installed":true, "enabled":true, "source":"/plugins/example.py",
            "declaration":null, "loaded":false, "host":"online", "runtime":null
        }]}),
        RequestPath::EnginePluginInstall | RequestPath::EnginePluginUninstall => {
            json!({"id":"example"})
        }
        RequestPath::SessionCreate | RequestPath::SessionGet | RequestPath::SessionArchive => {
            summary()
        }
        RequestPath::SessionConfigUpdate => summary()["session_config"].clone(),
        RequestPath::SessionConversationList => json!({
            "partition":{"target_id":"developer", "user_id":"user-1", "session_id":"session-1"},
            "offset":0, "next_offset":null, "messages":[{"role":"user", "content":"Hello"}]
        }),
        RequestPath::SessionConversationClear => json!({"cleared":true}),
        RequestPath::SessionExecutionStart => {
            json!({"execution_id":"execution-1", "accepted":true})
        }
        RequestPath::SessionExecutionSteer => {
            json!({"execution_id":"execution-1", "request_id":"request-1", "accepted":true})
        }
        RequestPath::SessionExecutionStop => {
            json!({"execution_id":"execution-1", "accepted":true, "status":"stopping"})
        }
        RequestPath::SessionExecutionGet => json!({"execution":null, "acceptance":{
            "request_id":"input-1", "certainty":"unknown", "execution_id":null, "input":null, "outcome":null
        }}),
        RequestPath::SessionSubscribe => json!({
            "subscription_id":"subscription-1", "stream_id":"stream-1", "snapshot_seq":12, "snapshot":summary(),
            "replay":{"max_events":4096, "max_bytes":16777216, "persistent":false}
        }),
        RequestPath::SessionUnsubscribe => {
            json!({"subscription_id":"subscription-1", "unsubscribed":true})
        }
        RequestPath::PluginWorkerHello
        | RequestPath::PluginContextCancel
        | RequestPath::PluginContextCompleteQuery
        | RequestPath::PluginStreamClose
        | RequestPath::PluginCallRelease => json!({}),
        RequestPath::PluginInspect => json!({"registration_mode":"direct","plugin_version":"1.0.0",
            "fae_plugin_api_version":"0.2.0","description":"Demo","engine":"py","mounts":[{"point":"SingleAgentHook","implementation":"Demo"}]}),
        RequestPath::PluginInvoke | RequestPath::PluginNext => json!("prompt"),
        RequestPath::PluginContextGet => summary(),
        RequestPath::PluginContextExport => json!({"user_info":summary()["user_info"],
            "session_config":summary()["session_config"],"memory_view":{"history":[],"history_offset":0,"summary":null,"user_memories":[]},
            "latest_execution_view":null}),
        RequestPath::PluginContextFork | RequestPath::PluginHostSession => context_reference(),
        RequestPath::PluginOutputSend => json!({"output_id":"output-1"}),
        RequestPath::PluginStreamNext => json!({"eof":true}),
        RequestPath::PluginHostEngine => json!({"handle":"engine-1"}),
        RequestPath::PluginContextStacks => json!([]),
    }
}

fn event_examples() -> Vec<(EventPath, Value)> {
    vec![
        (
            EventPath::ConfigUpdated,
            json!({"config":summary()["session_config"]}),
        ),
        (
            EventPath::ConversationAppended,
            json!({
                "execution_id":"execution-1", "invocation_id":"invocation-1",
                "partition":{"target_id":"developer", "user_id":"user-1", "session_id":"session-1"},
                "messages":[{"role":"user", "content":"Hello"}]
            }),
        ),
        (
            EventPath::ExecutionStateChanged,
            json!({"execution_id":"execution-1", "status":"finishing", "reason":null}),
        ),
        (
            EventPath::InputApplied,
            json!({"execution_id":"execution-1", "request_id":"input-1", "applied_at":"2026-11-23T01:00:00Z"}),
        ),
        (
            EventPath::InputNotApplied,
            json!({"execution_id":"execution-1", "request_id":"input-2", "reason":"stopped", "processed_at":"2026-11-23T01:00:00Z"}),
        ),
        (
            EventPath::InvocationChanged,
            json!({
                "execution_id":"execution-1", "invocation":{
                    "invocation_id":"invocation-1", "parent_invocation_id":null, "kind":"plan",
                    "plan_id":"plan-1", "task_id":null, "runtime_id":null,
                    "agent_id":"developer", "workflow_id":null, "node_id":null, "model":null, "status":"running"
                }
            }),
        ),
        (
            EventPath::OutputDelta,
            json!({
                "execution_id":"execution-1", "invocation_id":"invocation-1", "output_id":"output-1",
                "source":{"kind":"model"},
                "content":[{"type":"text", "text":"Hello"}]
            }),
        ),
        (
            EventPath::ExecutionFinished,
            json!({"execution_id":"execution-1", "outcome":{"type":"succeeded", "result":[{"type":"text", "text":"Done"}]}}),
        ),
    ]
}

#[test]
fn plugin_native_arguments_and_correlated_results_are_strict() {
    let frame = |method: &str, argument: Value| {
        json!({
            "version":4, "request_id":"call", "session_id":"session-1",
            "path":"/request/plugin/invoke",
            "payload":{"point":"SingleAgentHook","method":method,"location":null,
                "contexts":{"demo:0":{"binding":{"handle":"demo:0","execution_id":"e","invocation_id":"i"},
                    "session_id":"session-1","summary":null}},"argument":argument,
                "group":"session","timeout_ms":1000,"model_members":null}
        })
    };
    for (method, argument) in [
        ("on_memory", json!([{"content":"incomplete"}])),
        (
            "on_history",
            json!([{"role":"invalid","content":"history"}]),
        ),
        ("on_model_req", json!({"model":"demo"})),
        ("on_tools_req", json!({"arguments":{}})),
        ("on_model_resp", json!({"kind":"completed","response":{}})),
    ] {
        assert!(serde_json::from_value::<Message>(frame(method, argument)).is_err());
    }
    let request: Message = serde_json::from_value(frame("on_prompt", json!("input"))).unwrap();
    let invalid: ResponseMessage = serde_json::from_value(json!({
        "version":4, "request_id":"call","session_id":"session-1","path":"/response/plugin/invoke",
        "payload":{"code":0,"msg":"ok","data":{"unregistered":"result"}}
    }))
    .unwrap();
    assert!(invalid.validate_for(&request).is_err());
    let valid = ResponseMessage::success(
        &request,
        request.session_id.clone(),
        "ok",
        Some(ResponsePayload::PluginInvoke(json!("output"))),
    )
    .unwrap();
    valid.validate_for(&request).unwrap();
}

#[test]
fn every_registered_request_and_response_roundtrips_with_its_own_schema() {
    let examples = request_examples();
    assert_eq!(examples.len(), RequestPath::ALL.len());
    for (path, payload) in examples {
        let value = request(path.as_str(), payload);
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(message.path, MessagePath::Request(path));
        assert_eq!(message.as_request().unwrap().path(), path);
        assert_eq!(serde_json::to_value(&message).unwrap(), value);
        let mut response = json!({
            "version":4, "request_id":"request-1", "path":path.response().as_str(),
            "payload":{"code":0, "msg":"ok", "data":response_example(path)}
        });
        if path.is_session() {
            response["session_id"] = json!("session-1");
        }
        let decoded: ResponseMessage = serde_json::from_value(response.clone()).unwrap();
        assert_eq!(decoded.path.request(), path);
        assert_eq!(
            decoded.payload.data.as_ref().unwrap().path(),
            path.response()
        );
        assert_eq!(serde_json::to_value(decoded).unwrap(), response);
    }
}

#[test]
fn plugin_result_output_roundtrips_with_plugin_identity() {
    let value = json!({
        "execution_id":"execution-1", "invocation_id":"invocation-1",
        "output_id":"result", "source":{"kind":"plugin_result","plugin_id":"byte_agent_plan"},
        "content":[{"type":"text","text":"**models**"}]
    });
    let delta: OutputDelta = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(
        &delta.source,
        OutputSource::PluginResult { plugin_id } if plugin_id == "byte_agent_plan"
    ));
    assert_eq!(serde_json::to_value(delta).unwrap(), value);
}

#[test]
fn every_registered_event_roundtrips_and_cannot_be_dispatched_as_a_request() {
    let examples = event_examples();
    assert_eq!(examples.len(), EventPath::ALL.len());
    for (path, payload) in examples {
        let value = json!({
            "version":4, "session_id":"session-1", "stream_id":"stream-1", "seq":12,
            "path":path.as_str(), "payload":payload
        });
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(message.path, MessagePath::Event(path));
        assert_eq!(
            message.as_request().unwrap_err(),
            ProtocolError::ExternalEvent
        );
        assert_eq!(serde_json::to_value(message).unwrap(), value);
    }
}

#[test]
fn start_response_and_finished_match_the_design_golden_samples() {
    let value = json!({
        "version":4, "request_id":"req-start-1", "session_id":"session-1",
        "path":"/request/session/execution/start",
        "payload":{"input":[{"type":"text", "text":"继续实现登录"}]}
    });
    let message: Message = serde_json::from_value(value.clone()).unwrap();
    let generated = Message::request(
        RequestId::new("req-start-1").unwrap(),
        Some(SessionId::new("session-1").unwrap()),
        RequestPayload::SessionExecutionStart(ExecutionStartRequest {
            input: vec![ContentBlock::Text {
                text: "继续实现登录".into(),
            }],
            target: None,
            workspace: None,
            options: None,
        }),
    )
    .unwrap();
    assert_eq!(serde_json::to_value(generated).unwrap(), value);
    let response = ResponseMessage::success(
        &message,
        message.session_id.clone(),
        "已受理",
        Some(ResponsePayload::SessionExecutionStart(ExecutionAccepted {
            execution_id: ExecutionId::new("execution-1").unwrap(),
            accepted: true,
        })),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        json!({
            "version":4, "request_id":"req-start-1", "session_id":"session-1",
            "path":"/response/session/execution/start",
            "payload":{"code":0, "msg":"已受理", "data":{"execution_id":"execution-1", "accepted":true}}
        })
    );
    let event = Message::event(
        SessionId::new("session-1").unwrap(),
        "stream-1".into(),
        12,
        None,
        EventPayload::ExecutionFinished(ExecutionFinished {
            execution_id: ExecutionId::new("execution-1").unwrap(),
            outcome: crate::ExecutionOutcome::Succeeded {
                result: vec![ContentBlock::Text {
                    text: "完成".into(),
                }],
            },
        }),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(event).unwrap(),
        json!({
            "version":4, "session_id":"session-1", "stream_id":"stream-1", "seq":12,
            "path":"/event/session/execution/finished",
            "payload":{"execution_id":"execution-1", "outcome":{"type":"succeeded", "result":[{"type":"text", "text":"完成"}]}}
        })
    );
}

#[test]
fn response_codes_are_registered_integers_and_errors_always_have_null_data() {
    let request: Message =
        serde_json::from_value(request("/request/session/get", json!({}))).unwrap();
    for code in ResponseCode::ALL {
        let value = serde_json::to_value(code).unwrap();
        assert!(value.is_u64());
        assert_eq!(
            serde_json::from_value::<ResponseCode>(value).unwrap(),
            *code
        );
        let response = ResponseMessage::error(&request, *code, "result").unwrap();
        let value = serde_json::to_value(response).unwrap();
        assert!(value["payload"].as_object().unwrap().contains_key("data"));
        assert!(value["payload"]["data"].is_null());
        assert_eq!(value["payload"].as_object().unwrap().len(), 3);
        serde_json::from_value::<ResponseMessage>(value).unwrap();
    }
    for invalid in [
        json!("409"),
        json!(409.0),
        json!(-1),
        json!(401),
        json!(200),
        Value::Null,
    ] {
        assert!(serde_json::from_value::<ResponseCode>(invalid).is_err());
    }
}

#[test]
fn requests_reject_missing_identity_stream_fields_and_top_level_execution() {
    let valid = request("/request/session/execution/start", json!({"input":[]}));
    for field in ["request_id", "session_id"] {
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<Message>(value).is_err(), "{field}");
    }
    for (field, extra) in [
        ("execution_id", json!("execution-1")),
        ("stream_id", json!("stream-1")),
        ("seq", json!(12)),
        ("stream_id", Value::Null),
        ("seq", Value::Null),
        ("user_id", json!("forged")),
    ] {
        let mut value = valid.clone();
        value[field] = extra;
        assert!(serde_json::from_value::<Message>(value).is_err(), "{field}");
    }
    for field in ["request_id", "session_id"] {
        let mut value = valid.clone();
        value[field] = json!("");
        assert!(serde_json::from_value::<Message>(value).is_err());
    }
}

#[test]
fn events_require_session_stream_and_unsigned_sequence() {
    let (path, payload) = event_examples().pop().unwrap();
    let valid = json!({"version":4, "session_id":"session-1", "stream_id":"stream-1", "seq":12,
        "path":path.as_str(), "payload":payload});
    for field in ["session_id", "stream_id", "seq"] {
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<Message>(value).is_err(), "{field}");
    }
    for invalid in [json!(0), json!(-1), json!(1.5), json!("12"), Value::Null] {
        let mut value = valid.clone();
        value["seq"] = invalid;
        assert!(serde_json::from_value::<Message>(value).is_err());
    }
}

#[test]
fn path_selects_schema_without_untagged_payload_guessing_or_extension_routes() {
    for path in [
        "/request/session/execution/steer",
        "/request/session/execution/stop",
    ] {
        assert!(serde_json::from_value::<Message>(request(path, json!({"input":[]}))).is_err());
    }
    for path in [
        "/request/session/open",
        "/request/session/input",
        "/request/session/close",
        "/request/anything",
        "/request/plugin/host/arbitrary",
    ] {
        assert!(serde_json::from_value::<Message>(request(path, json!({}))).is_err());
    }
    let mut valid: Message =
        serde_json::from_value(request("/request/engine/status", json!({}))).unwrap();
    valid.path = MessagePath::Request(RequestPath::EngineShutdown);
    assert!(serde_json::to_value(valid).is_err());
    let mut response = json!({"version":4, "request_id":"request-1",
        "path":"/response/engine/status", "payload":{"code":0, "msg":"ok", "data":{"accepted":true}}});
    assert!(serde_json::from_value::<ResponseMessage>(response.clone()).is_err());
    response["payload"] =
        json!({"code":409, "msg":"busy", "data":response_example(RequestPath::EngineStatus)});
    assert!(serde_json::from_value::<ResponseMessage>(response).is_err());
}

#[test]
fn only_v4_is_accepted_and_old_envelopes_are_rejected() {
    for version in [0, 1, 2, 3, 5] {
        let mut value = request("/request/engine/status", json!({}));
        value["version"] = json!(version);
        assert!(serde_json::from_value::<Message>(value).is_err());
    }
    assert!(
        serde_json::from_value::<Message>(request(
            "/request/engine/hello",
            json!({"versions":[3]})
        ))
        .is_err()
    );
    for legacy in [
        json!({"version":3, "request_id":"request-1", "type":"hello"}),
        json!({"version":3, "request_id":"request-1", "type":"input", "data":{"input":{"new_chat":{"text":"hello"}}}}),
        json!({"version":3, "instance_id":"old", "request_id":"request-1", "type":"closed"}),
    ] {
        assert!(serde_json::from_value::<Message>(legacy.clone()).is_err());
        assert!(serde_json::from_value::<ResponseMessage>(legacy).is_err());
    }
}

#[test]
fn responses_reject_stream_fields_missing_body_keys_and_invalid_created_identity() {
    let valid = json!({"version":4, "request_id":"request-1", "path":"/response/engine/status",
        "payload":{"code":0, "msg":"ok", "data":null}});
    for field in ["code", "msg", "data"] {
        let mut value = valid.clone();
        value["payload"].as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<ResponseMessage>(value).is_err(),
            "{field}"
        );
    }
    for field in ["stream_id", "seq", "execution_id"] {
        let mut value = valid.clone();
        value[field] = Value::Null;
        assert!(
            serde_json::from_value::<ResponseMessage>(value).is_err(),
            "{field}"
        );
    }
    let value = json!({"version":4, "request_id":"request-1", "path":"/response/session/create",
        "session_id":"different", "payload":{"code":0, "msg":"ok", "data":summary()}});
    assert!(serde_json::from_value::<ResponseMessage>(value).is_err());
}

#[test]
fn subscription_cursor_and_session_options_have_explicit_constraints() {
    for payload in [
        json!({"after_seq":12}),
        json!({"stream_id":"stream-1"}),
        json!({"stream_id":"", "after_seq":12}),
    ] {
        assert!(
            serde_json::from_value::<Message>(request("/request/session/subscribe", payload))
                .is_err()
        );
    }
    for payload in [
        json!({"input":[], "options":{"model":"forged"}}),
        json!({"input":[], "auto_steer":true}),
    ] {
        assert!(
            serde_json::from_value::<Message>(request("/request/session/execution/start", payload))
                .is_err()
        );
    }
}

#[test]
fn golden_fixtures_are_shared_with_sdk_consumers() {
    let request: Message =
        serde_json::from_str(include_str!("../../tests/fixtures/protocol/start.json")).unwrap();
    let response: ResponseMessage = serde_json::from_str(include_str!(
        "../../tests/fixtures/protocol/start-response.json"
    ))
    .unwrap();
    let event: Message =
        serde_json::from_str(include_str!("../../tests/fixtures/protocol/finished.json")).unwrap();
    assert_eq!(response.request_id, request.request_id.unwrap());
    assert_eq!(response.session_id, request.session_id);
    assert_eq!(event.session_id, response.session_id);
}

#[test]
fn obsolete_envelopes_report_version_rejection_before_missing_new_fields() {
    let legacy = r#"{"version":3,"request_id":"request-1","type":"hello"}"#;
    assert!(
        serde_json::from_str::<Message>(legacy)
            .unwrap_err()
            .to_string()
            .contains("unsupported protocol version 3")
    );
    assert!(
        serde_json::from_str::<ResponseMessage>(legacy)
            .unwrap_err()
            .to_string()
            .contains("unsupported protocol version 3")
    );
}

#[test]
fn response_construction_cannot_change_the_requested_session() {
    let request: Message =
        serde_json::from_value(request("/request/session/get", json!({}))).unwrap();
    assert!(
        ResponseMessage::success(&request, Some(SessionId::new("other").unwrap()), "ok", None)
            .is_err()
    );
}

#[test]
fn server_frames_accept_only_valid_responses_and_events() {
    for value in [
        request("/request/engine/status", json!({})),
        json!({"version":3,"type":"closed"}),
        json!({"version":4,"request_id":"request","path":"/response/engine/status",
            "payload":{"code":409,"msg":"busy","data":{"sessions":0,"active_executions":0,"shutting_down":false}}}),
    ] {
        assert!(serde_json::from_value::<ServerFrame>(value).is_err());
    }
    let response: ServerFrame = serde_json::from_str(include_str!(
        "../../tests/fixtures/protocol/start-response.json"
    ))
    .unwrap();
    assert!(matches!(response, ServerFrame::Response(_)));
    let event: ServerFrame =
        serde_json::from_str(include_str!("../../tests/fixtures/protocol/finished.json")).unwrap();
    assert!(matches!(event, ServerFrame::Event(_)));
}
