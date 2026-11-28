use super::*;
use crate::{RequestId, SessionId};
use serde_json::{Value, json};

fn request(event: &str, payload: Value) -> Value {
    json!({"api_version":"0.1.0","request_id":"request_1","event":event,
        "payload":payload,"session":if event.starts_with("/request/session/") && event != "/request/session/create" {
            json!({"session_id":"session_1"})
        } else { Value::Null }, "plan":null})
}
fn status() -> Message {
    serde_json::from_value(request("/request/engine/status", json!({}))).unwrap()
}
#[test]
fn public_raw_requests_reject_worker_events_and_preserve_json() {
    let mut value = request("/request/plan/call", json!([null, {"env":{"payload":1}}]));
    value["plan"] = json!({"plan_id":"report_plan"});
    let raw: RequestMessage = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(raw.payload, value["payload"]);
    let message = raw.into_message().unwrap();
    assert_eq!(serde_json::to_value(message).unwrap(), value);
    value["binding"] = json!({});
    assert!(serde_json::from_value::<RequestMessage>(value).is_err());
    for event in [
        "/request/plugin/inspect",
        "/request/plugin/plan/create",
        "/request/plugin/context/get",
    ] {
        assert!(serde_json::from_value::<RequestMessage>(request(event, json!({}))).is_err());
    }
}
#[test]
fn public_session_targets_are_selected_by_plan_and_saved_with_config() {
    let mut value = request(
        "/request/session/create",
        json!({"name":"Report","format":"markdown"}),
    );
    value["plan"] = json!({"plan_id":"report_plan"});
    let raw: RequestMessage = serde_json::from_value(value.clone()).unwrap();
    let message = raw.into_message().unwrap();
    let RequestPayload::SessionCreate(create) = message.as_request().unwrap() else {
        panic!()
    };
    assert_eq!(
        create.target,
        crate::PlanTarget::PluginPlan {
            plan_id: "report_plan".into(),
            config: json!({"format":"markdown"})
        }
    );
    let serialized = serde_json::to_value(&message).unwrap();
    assert!(serialized["payload"].get("target").is_none());
    assert_eq!(serialized["payload"]["format"], "markdown");
    let roundtrip: Message = serde_json::from_value(serialized).unwrap();
    assert_eq!(
        serde_json::to_value(roundtrip).unwrap()["plan"],
        value["plan"]
    );
    value["payload"]["target"] = json!({"type":"agent","agent_id":"other"});
    assert!(serde_json::from_value::<RequestMessage>(value).is_err());
    let mut value = request(
        "/request/session/execution/start",
        json!({"anything":[1,2]}),
    );
    value["plan"] = json!({"plan_id":"report_plan"});
    let raw: RequestMessage = serde_json::from_value(value).unwrap();
    let message = raw.into_message().unwrap();
    let RequestPayload::SessionExecutionStart(start) = message.as_request().unwrap() else {
        panic!()
    };
    assert_eq!(
        start.target,
        Some(crate::PlanTarget::PluginPlan {
            plan_id: "report_plan".into(),
            config: json!({"anything":[1,2]})
        })
    );
}
#[test]
fn response_is_correlated_without_an_event_or_nested_body() {
    let request = status();
    let response = ResponseMessage::success(
        &request,
        None,
        "ok",
        Some(ResponsePayload::EngineStatus(EngineStatus {
            sessions: 1,
            active_executions: 0,
            shutting_down: false,
        })),
    )
    .unwrap();
    let wire = serde_json::to_value(&response).unwrap();
    assert_eq!(
        wire,
        json!({"api_version":"0.1.0","response_id":"request_1",
        "code":0,"msg":"ok","payload":{"sessions":1,"active_executions":0,"shutting_down":false},
        "session":null,"plan":null})
    );
    let decoded: ResponseMessage = serde_json::from_value(wire).unwrap();
    decoded.validate_for(&request).unwrap();
    assert!(matches!(
        decoded.decode_for(&request).unwrap(),
        Some(ResponsePayload::EngineStatus(_))
    ));
}
#[test]
fn decoding_a_response_does_not_guess_the_business_contract() {
    let request = status();
    let wire = json!({"api_version":"0.1.0","response_id":"request_1","code":0,
        "msg":"ok","payload":{"accepted":true},"session":null,"plan":null});
    let response: ResponseMessage = serde_json::from_value(wire).unwrap();
    assert!(response.validate_for(&request).is_err());
}
#[test]
fn response_rejects_wrong_pending_id_session_and_plan() {
    let request = status();
    let response = ResponseMessage::error(&request, ResponseCode::Conflict, "busy").unwrap();
    for (field, value) in [
        ("response_id", json!("other")),
        ("session", json!({"session_id":"other"})),
        ("plan", json!({"plan_id":"other_plan"})),
    ] {
        let mut wire = serde_json::to_value(&response).unwrap();
        wire[field] = value;
        let response: ResponseMessage = serde_json::from_value(wire).unwrap();
        assert!(response.validate_for(&request).is_err());
    }
}
#[test]
fn requests_require_all_six_fields_and_reject_old_or_internal_fields() {
    let valid = request("/request/engine/status", json!({}));
    for field in [
        "api_version",
        "request_id",
        "event",
        "payload",
        "session",
        "plan",
    ] {
        let mut wire = valid.clone();
        wire.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<Message>(wire).is_err(), "{field}");
    }
    for field in [
        "version",
        "path",
        "session_id",
        "binding",
        "response_id",
        "code",
        "msg",
        "stream_id",
        "seq",
    ] {
        let mut wire = valid.clone();
        wire[field] = Value::Null;
        assert!(serde_json::from_value::<Message>(wire).is_err(), "{field}");
    }
    for version in [json!(4), json!("0.2.0"), json!("0.1"), Value::Null] {
        let mut wire = valid.clone();
        wire["api_version"] = version;
        assert!(serde_json::from_value::<Message>(wire).is_err());
    }
}
#[test]
fn response_requires_seven_fields_and_error_payload_is_null() {
    let response = ResponseMessage::error(&status(), ResponseCode::Conflict, "busy").unwrap();
    let valid = serde_json::to_value(response).unwrap();
    assert_eq!(valid.as_object().unwrap().len(), 7);
    for field in [
        "api_version",
        "response_id",
        "code",
        "msg",
        "payload",
        "session",
        "plan",
    ] {
        let mut wire = valid.clone();
        wire.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<ResponseMessage>(wire).is_err(),
            "{field}"
        );
    }
    for field in [
        "event",
        "request_id",
        "data",
        "binding",
        "call",
        "stream_id",
        "seq",
    ] {
        let mut wire = valid.clone();
        wire[field] = Value::Null;
        assert!(serde_json::from_value::<ResponseMessage>(wire).is_err());
    }
    let mut wire = valid;
    wire["payload"] = json!({"business":true});
    assert!(serde_json::from_value::<ResponseMessage>(wire).is_err());
}
#[test]
fn response_codes_are_registered_unsigned_integers() {
    for code in ResponseCode::ALL {
        assert_eq!(
            serde_json::from_value::<ResponseCode>(serde_json::to_value(code).unwrap()).unwrap(),
            *code
        );
    }
    for invalid in [
        json!("409"),
        json!(409.0),
        json!(-1),
        json!(401),
        json!(true),
        Value::Null,
    ] {
        assert!(serde_json::from_value::<ResponseCode>(invalid).is_err());
    }
}
#[test]
fn notifications_have_nested_cursors_and_cannot_execute() {
    let message = Message::event(
        SessionId::new("session_1").unwrap(),
        "stream_1".into(),
        1,
        None,
        EventPayload::ConfigUpdated(ConfigUpdated {
            config: serde_json::from_value(json!({
                "session_id":"session_1","name":"Session","lifecycle":"open",
                "created_at":"2026-11-23T01:00:00Z","updated_at":"2026-11-23T01:00:00Z",
                "target":{"type":"agent","agent_id":"developer"},
                "workspace":{"root":"/workspace","project_files":[]},"options":{},"channels":[]
            }))
            .unwrap(),
        }),
    )
    .unwrap();
    let wire = serde_json::to_value(&message).unwrap();
    assert_eq!(
        wire["session"],
        json!({"session_id":"session_1","stream_id":"stream_1","seq":1})
    );
    let decoded: Message = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(
        decoded.as_request().unwrap_err(),
        ProtocolError::ExternalEvent
    );
    for field in ["session_id", "stream_id", "seq"] {
        let mut invalid = wire.clone();
        invalid["session"].as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<Message>(invalid).is_err());
    }
    for seq in [json!(0), json!(-1), json!(1.5), json!("1"), Value::Null] {
        let mut invalid = wire.clone();
        invalid["session"]["seq"] = seq;
        assert!(serde_json::from_value::<Message>(invalid).is_err());
    }
    let mut invalid = wire;
    invalid["response_id"] = json!("request_1");
    assert!(serde_json::from_value::<ServerFrame>(invalid).is_err());
}
#[test]
fn request_session_cannot_accept_a_notification_cursor() {
    let mut wire = request("/request/session/get", json!({}));
    wire["session"]["seq"] = json!(1);
    assert!(serde_json::from_value::<Message>(wire).is_err());
}
#[test]
fn hello_is_an_empty_request_and_reports_fixed_api_version() {
    serde_json::from_value::<Message>(request("/request/engine/hello", json!({}))).unwrap();
    assert!(
        serde_json::from_value::<Message>(request(
            "/request/engine/hello",
            json!({"versions":[4]})
        ))
        .is_err()
    );
}
#[test]
fn request_event_selects_schema_and_rejects_unknown_routes() {
    for event in [
        "/request/session/execution/steer",
        "/request/session/execution/stop",
        "/request/anything",
        "/request/session/open",
    ] {
        assert!(serde_json::from_value::<Message>(request(event, json!({"input":[]}))).is_err());
    }
    let mut message = status();
    message.event = MessageEvent::Request(RequestEvent::EngineShutdown);
    assert!(serde_json::to_value(message).is_err());
}
#[test]
fn constructors_preserve_session_identity() {
    let request = Message::request(
        RequestId::new("request_1").unwrap(),
        Some(SessionId::new("session_1").unwrap()),
        RequestPayload::SessionGet(Empty {}),
    )
    .unwrap();
    assert!(
        ResponseMessage::success(&request, Some(SessionId::new("other").unwrap()), "ok", None)
            .is_err()
    );
}
#[test]
fn server_frames_reject_requests_and_accept_raw_responses() {
    assert!(
        serde_json::from_value::<ServerFrame>(request("/request/engine/status", json!({})))
            .is_err()
    );
    let response = ResponseMessage::error(&status(), ResponseCode::Conflict, "busy").unwrap();
    assert!(matches!(
        serde_json::from_value::<ServerFrame>(serde_json::to_value(response).unwrap()).unwrap(),
        ServerFrame::Response(_)
    ));
}

#[test]
fn null_business_result_is_preserved_for_registered_provider_calls() {
    let request = serde_json::from_value::<RequestMessage>(json!({
        "api_version":"0.1.0","request_id":"null-result","event":"/request/plan/call",
        "session":null,"plan":{"plan_id":"report_plan"},"payload":null
    }))
    .unwrap()
    .into_message()
    .unwrap();
    let response = ResponseMessage::success(
        &request,
        None,
        "ok",
        Some(ResponsePayload::PlanCall(Value::Null)),
    )
    .unwrap();
    assert!(matches!(
        response.decode_for(&request).unwrap(),
        Some(ResponsePayload::PlanCall(Value::Null))
    ));
    response.validate_for(&request).unwrap();
}
