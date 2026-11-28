include!("support/service_fixture.rs");
use fae_agent::{
    ExecutionId, ExecutionOutcome, ExecutionStatus, PlanTarget, RequestId, SessionId,
    protocol::{
        AcceptanceCertainty, ConfigUpdateRequest, ContentBlock, ConversationListRequest, Empty,
        EventPayload, ExecutionGetRequest, ExecutionStartRequest, ExecutionSteerRequest,
        ExecutionStopRequest, Message as ProtocolMessage, MessagePayload, RequestPayload,
        ResponseCode, ResponseMessage, ResponsePayload, ServerFrame, SessionCreateRequest,
        SubscribeRequest,
    },
};

fn create(service: &Service, target: PlanTarget) -> SessionCreateRequest {
    SessionCreateRequest {
        name: "test session".into(),
        target,
        workspace: Some(service.home.to_string_lossy().into()),
        options: Default::default(),
        channels: vec![],
    }
}
async fn agent(service: &Service) -> Client {
    Client::open(
        &service.home,
        &service.config,
        create(
            service,
            PlanTarget::Agent {
                agent_id: "fae".into(),
            },
        ),
        None,
        None,
        None,
    )
    .await
    .unwrap()
}
async fn workflow(service: &Service, id: &str) -> Client {
    Client::open(
        &service.home,
        &service.config,
        create(
            service,
            PlanTarget::Workflow {
                workflow_id: id.into(),
            },
        ),
        None,
        None,
        None,
    )
    .await
    .unwrap()
}
fn start_payload(input: Vec<ContentBlock>) -> RequestPayload {
    RequestPayload::SessionExecutionStart(ExecutionStartRequest {
        input,
        target: None,
        workspace: None,
        options: None,
    })
}
async fn start_agent(client: &mut Client, text: &str) -> ExecutionId {
    let ResponsePayload::SessionExecutionStart(accepted) = client
        .request(start_payload(client::text_content(text)))
        .await
        .unwrap()
    else {
        panic!()
    };
    accepted.execution_id
}
async fn start_workflow(client: &mut Client) -> ExecutionId {
    let ResponsePayload::SessionExecutionStart(accepted) = client
        .request(start_payload(vec![ContentBlock::Json { value: json!({}) }]))
        .await
        .unwrap()
    else {
        panic!()
    };
    accepted.execution_id
}
async fn finish(client: &mut Client) -> (ExecutionId, ExecutionOutcome) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let MessagePayload::Event(EventPayload::ExecutionFinished(finished)) =
                client.next().await.unwrap().payload
            {
                return (finished.execution_id, finished.outcome);
            }
        }
    })
    .await
    .unwrap()
}
async fn exchange(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    request: &ProtocolMessage,
) -> ResponseMessage {
    socket
        .send(Message::Text(
            serde_json::to_string(request).unwrap().into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => {
                    if let ServerFrame::Response(response) = serde_json::from_str(&text).unwrap() {
                        if Some(&response.response_id) == request.request_id.as_ref() {
                            return response;
                        }
                    }
                }
                Message::Ping(data) => socket.send(Message::Pong(data)).await.unwrap(),
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workflow_create_subscribe_and_get_never_launch_execution() {
    let mut service = Service::start(None).await;
    service.write_workflow(
        "explicit",
        "open('launched', 'w').write('yes')\nresult = 42",
    );
    let mut client = workflow(&service, "explicit").await;
    assert!(client.info.active_execution.is_none());
    assert!(!client.info.context.memory.history_loaded);
    let ResponsePayload::SessionGet(summary) = client
        .request(RequestPayload::SessionGet(Empty {}))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(summary.active_execution.is_none());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!service.home.join("launched").exists());
    let execution = start_workflow(&mut client).await;
    let (finished, outcome) = finish(&mut client).await;
    assert_eq!(finished, execution);
    assert_eq!(client::outcome_result(outcome).unwrap(), json!(42));
    assert!(service.home.join("launched").exists());
    let ResponsePayload::SessionExecutionGet(result) = client
        .request(RequestPayload::SessionExecutionGet(ExecutionGetRequest {
            execution_id: Some(execution),
            input_request_id: None,
        }))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(result.execution.unwrap().status, ExecutionStatus::Succeeded);
    let next = start_workflow(&mut client).await;
    assert_eq!(finish(&mut client).await.0, next);
    let ResponsePayload::SessionArchive(archived) = client
        .request(RequestPayload::SessionArchive(Empty {}))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        archived.session_config.lifecycle,
        fae_agent::SessionLifecycle::Archived
    );
    assert!(
        client
            .request(start_payload(vec![ContentBlock::Json { value: json!({}) }]))
            .await
            .is_err()
    );
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsubscribe_keeps_execution_running_and_replays_public_events() {
    let mut service = Service::start(None).await;
    service.write_workflow(
        "replay",
        "import time\ntime.sleep(0.3)\nopen('replayed', 'w').write('once')\nresult = 42",
    );
    let mut original = workflow(&service, "replay").await;
    let sid = original.sid().to_string();
    let stream = original.stream.clone();
    let execution = start_workflow(&mut original).await;
    original.close().await.unwrap();
    drop(original);
    let mut resumed = Client::open(
        &service.home,
        &service.config,
        create(
            &service,
            PlanTarget::Workflow {
                workflow_id: "replay".into(),
            },
        ),
        Some(sid),
        None,
        Some(SubscribeRequest {
            stream_id: Some(stream),
            after_seq: Some(0),
        }),
    )
    .await
    .unwrap();
    let (id, outcome) = finish(&mut resumed).await;
    assert_eq!(id, execution);
    assert!(matches!(outcome, ExecutionOutcome::Succeeded { .. }));
    assert!(service.home.join("replayed").exists());
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_subscribers_observe_same_execution_and_unsubscribe_is_connection_scoped() {
    let mut service = Service::start(None).await;
    service.write_workflow("shared", "import time\ntime.sleep(0.2)\nresult = 42");
    let mut first = workflow(&service, "shared").await;
    let mut second = Client::open(
        &service.home,
        &service.config,
        create(
            &service,
            PlanTarget::Workflow {
                workflow_id: "shared".into(),
            },
        ),
        Some(first.sid().into()),
        None,
        None,
    )
    .await
    .unwrap();
    assert_ne!(first.subscription, second.subscription);
    assert!(
        second
            .request(RequestPayload::SessionUnsubscribe(
                fae_agent::protocol::UnsubscribeRequest {
                    subscription_id: first.subscription.clone()
                }
            ))
            .await
            .is_err()
    );
    let execution = start_workflow(&mut first).await;
    assert_eq!(finish(&mut first).await.0, execution);
    assert_eq!(finish(&mut second).await.0, execution);
    first.close().await.unwrap();
    let next = start_workflow(&mut second).await;
    assert_eq!(finish(&mut second).await.0, next);
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_ids_are_correlated_and_conflicts_do_not_execute_another_operation() {
    let mut service = Service::start(None).await;
    service.write_workflow("dedup", "result = 42");
    let client = workflow(&service, "dedup").await;
    let mut socket = client::connect(&service.home, &service.config, "/api/0.1.0/session")
        .await
        .unwrap();
    let request = ProtocolMessage::request(
        RequestId::new("same").unwrap(),
        Some(SessionId::new(client.sid()).unwrap()),
        RequestPayload::SessionGet(Empty {}),
    )
    .unwrap();
    let first = exchange(&mut socket, &request).await;
    let repeated = exchange(&mut socket, &request).await;
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(repeated).unwrap()
    );
    let conflict = ProtocolMessage::request(
        RequestId::new("same").unwrap(),
        request.session_id.clone(),
        RequestPayload::SessionArchive(Empty {}),
    )
    .unwrap();
    let response = exchange(&mut socket, &conflict).await;
    assert_eq!(response.code, ResponseCode::Conflict);
    assert!(response.payload.is_null());
    response.validate_for(&conflict).unwrap();
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn old_routes_versions_and_external_events_are_rejected() {
    let mut service = Service::start(None).await;
    for route in ["/v3/control", "/v3/session", "/v3/lark/session"] {
        assert!(
            client::connect(&service.home, &service.config, route)
                .await
                .is_err()
        );
    }
    for value in [
        json!({"version":3,"request_id":"r","type":"status"}),
        json!({"version":4,"request_id":"r","type":"status"}),
        json!({"version":4,"request_id":"r","path":"/request/session/not_registered","payload":{}}),
        json!({"version":4,"session_id":"s","stream_id":"stream","seq":1,"path":"/event/session/execution/state_changed","payload":{"execution_id":"e","status":"running","reason":null}}),
    ] {
        let mut socket = client::connect(&service.home, &service.config, "/api/0.1.0/session")
            .await
            .unwrap();
        socket
            .send(Message::Text(value.to_string().into()))
            .await
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap();
        assert!(!matches!(response, Some(Ok(Message::Text(_)))));
    }
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execution_limits_keep_queries_responsive_and_busy_start_is_never_steered() {
    let mut service = Service::start(None).await;
    service.write_workflow("slow", "import time\ntime.sleep(30)\nresult = 42");
    let mut clients = vec![];
    for _ in 0..8 {
        let mut client = workflow(&service, "slow").await;
        start_workflow(&mut client).await;
        clients.push(client);
    }
    assert!(
        clients[0]
            .request(start_payload(vec![ContentBlock::Json { value: json!({}) }]))
            .await
            .unwrap_err()
            .to_string()
            .contains("session_busy")
    );
    let mut rejected = workflow(&service, "slow").await;
    assert!(
        rejected
            .request(start_payload(vec![ContentBlock::Json { value: json!({}) }]))
            .await
            .unwrap_err()
            .to_string()
            .contains("resource_exhausted")
    );
    let ResponsePayload::SessionGet(summary) = tokio::time::timeout(
        Duration::from_secs(2),
        clients[0].request(RequestPayload::SessionGet(Empty {})),
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!()
    };
    assert!(summary.active_execution.is_some());
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_use_public_content_multiple_starts_stop_and_existing_history_partition() {
    let (base, mut requests, model_task) = model().await;
    let mut service = Service::start(Some(&base)).await;
    service.init("fae").await;
    let mut client = agent(&service).await;
    for text in ["hello", "again"] {
        let id = start_agent(&mut client, text).await;
        assert_eq!(requests.recv().await.as_deref(), Some(text));
        assert_eq!(finish(&mut client).await.0, id);
    }
    let ResponsePayload::SessionConversationList(page) = client
        .request(RequestPayload::SessionConversationList(
            ConversationListRequest {
                offset: 0,
                limit: 100,
            },
        ))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(!page.messages.is_empty());
    assert_eq!(page.partition.session_id.as_str(), client.sid());
    assert!(
        service
            .home
            .join(format!(
                "session/fae/{}/{}.jsonl",
                page.partition.user_id,
                client.sid()
            ))
            .exists()
    );
    let id = start_agent(&mut client, "stream then wait").await;
    assert_eq!(requests.recv().await.as_deref(), Some("stream then wait"));
    loop {
        if matches!(
            client.next().await.unwrap().payload,
            MessagePayload::Event(EventPayload::OutputDelta(_))
        ) {
            break;
        }
    }
    assert!(
        client
            .request(start_payload(client::text_content("must not steer")))
            .await
            .unwrap_err()
            .to_string()
            .contains("session_busy")
    );
    client
        .request(RequestPayload::SessionExecutionSteer(
            ExecutionSteerRequest {
                execution_id: ExecutionId::new("stale").unwrap(),
                input: client::text_content("stale"),
            },
        ))
        .await
        .unwrap_err();
    client
        .request(RequestPayload::SessionExecutionStop(ExecutionStopRequest {
            execution_id: id.clone(),
            reason: Some("test stop".into()),
        }))
        .await
        .unwrap();
    assert!(matches!(
        finish(&mut client).await.1,
        ExecutionOutcome::Cancelled { .. }
    ));
    assert!(!client.stop_round(Some(id.to_string())).await.unwrap());
    client
        .request(RequestPayload::SessionConversationClear(Empty {}))
        .await
        .unwrap();
    let ResponsePayload::SessionConversationList(page) = client
        .request(RequestPayload::SessionConversationList(
            ConversationListRequest {
                offset: 0,
                limit: 100,
            },
        ))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(page.messages.is_empty());
    service.stop().await;
    model_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_workspace_changes_affect_next_execution_and_keep_active_settings_fixed() {
    let mut service = Service::start(None).await;
    service.write_workflow(
        "workspace",
        "import time\ntime.sleep(0.3)\nopen('execution-workspace', 'w').write('done')\nresult = 42",
    );
    let next = service.home.join("next");
    std::fs::create_dir(&next).unwrap();
    let mut client = workflow(&service, "workspace").await;
    let id = start_workflow(&mut client).await;
    assert!(
        client
            .request(RequestPayload::SessionConfigUpdate(ConfigUpdateRequest {
                workspace: Some(next.to_string_lossy().into()),
                ..Default::default()
            }))
            .await
            .is_err()
    );
    let ResponsePayload::SessionExecutionGet(result) = client
        .request(RequestPayload::SessionExecutionGet(ExecutionGetRequest {
            execution_id: Some(id),
            input_request_id: None,
        }))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        result.execution.unwrap().effective.workspace,
        service.home.to_string_lossy()
    );
    finish(&mut client).await;
    assert!(service.home.join("execution-workspace").exists());
    assert!(!next.join("execution-workspace").exists());
    client
        .request(RequestPayload::SessionConfigUpdate(ConfigUpdateRequest {
            workspace: Some(next.to_string_lossy().into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    start_workflow(&mut client).await;
    finish(&mut client).await;
    assert!(next.join("execution-workspace").exists());
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_resume_does_not_create_or_start_a_session() {
    let mut service = Service::start(None).await;
    service.write_workflow("expired", "result = 42");
    let client = workflow(&service, "expired").await;
    let error = Client::open(
        &service.home,
        &service.config,
        create(
            &service,
            PlanTarget::Workflow {
                workflow_id: "expired".into(),
            },
        ),
        Some(client.sid().into()),
        None,
        Some(SubscribeRequest {
            stream_id: Some("expired".into()),
            after_seq: Some(0),
        }),
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("resume_expired"));
    assert!(client.info.active_execution.is_none());
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lark_identity_is_host_metadata_and_public_request_id_tracks_acceptance() {
    let (base, mut requests, model_task) = model().await;
    let mut service = Service::start(Some(&base)).await;
    service.init("fae").await;
    std::fs::write(service.home.join("lark.config"), serde_json::to_vec(&json!({
        "version":1,"app_id":"cli_test","app_secret":"test-secret","tenant_key":"tenant","agent_id":"fae","workspace":service.home
    })).unwrap()).unwrap();
    let principal = fae::session_host::LarkPrincipal {
        session_mapping_version: 2,
        thread_id: "omt_public".into(),
        channel: "lark".into(),
        user_id: "alice".into(),
        app_id: "cli_test".into(),
        tenant_key: "tenant".into(),
        chat_type: "group".into(),
        chat_id: "group".into(),
    };
    let create = create(
        &service,
        PlanTarget::Agent {
            agent_id: "fae".into(),
        },
    );
    let mut alice = Client::open(
        &service.home,
        &service.config,
        create.clone(),
        Some("omt_public".into()),
        Some(principal.clone()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(alice.info.user_info.user_id.as_str(), "alice");
    assert_eq!(alice.sid(), "omt_public");
    let rid = fae::lark::identity::receipt_id("cli_test", "tenant", "message");
    let payload = start_payload(client::text_content("hello"));
    let ResponsePayload::SessionExecutionStart(accepted) = alice
        .request_with_id(payload.clone(), RequestId::new(rid.clone()).unwrap())
        .await
        .unwrap()
    else {
        panic!()
    };
    let repeated = alice
        .request_with_id(payload, RequestId::new(rid.clone()).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        repeated,
        ResponsePayload::SessionExecutionStart(_)
    ));
    assert_eq!(requests.recv().await.as_deref(), Some("hello"));
    finish(&mut alice).await;
    let ResponsePayload::SessionExecutionGet(result) = alice
        .request(RequestPayload::SessionExecutionGet(ExecutionGetRequest {
            execution_id: None,
            input_request_id: Some(RequestId::new(rid).unwrap()),
        }))
        .await
        .unwrap()
    else {
        panic!()
    };
    let fact = result.acceptance.unwrap();
    assert_eq!(fact.certainty, AcceptanceCertainty::Known);
    assert_eq!(fact.execution_id, Some(accepted.execution_id));
    assert!(fact.outcome.is_some());
    let mut bob_principal = principal.clone();
    bob_principal.user_id = "bob".into();
    let bob = Client::open(
        &service.home,
        &service.config,
        create.clone(),
        Some("omt_public".into()),
        Some(bob_principal),
        None,
    )
    .await
    .unwrap();
    assert_eq!(bob.info.user_info.user_id.as_str(), "bob");
    assert!(
        Client::open(
            &service.home,
            &service.config,
            create,
            Some("omt_public".into()),
            None,
            None
        )
        .await
        .is_err()
    );
    service.stop().await;
    model_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timer_keeps_persisted_ownership_after_public_session_clean_and_disconnect() {
    let (base, _requests, model_task) = model().await;
    let mut service = Service::start(Some(&base)).await;
    service.init("fae").await;
    let path = service.home.join("agents/fae_config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["agent"]["user_id"] = json!("alice");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut client = agent(&service).await;
    let original = client.sid().to_string();
    start_agent(&mut client, "timer:create:once").await;
    let mut tool_outputs = Vec::new();
    loop {
        match client.next().await.unwrap().payload {
            MessagePayload::Event(EventPayload::OutputDelta(delta)) => {
                tool_outputs.extend(delta.content)
            }
            MessagePayload::Event(EventPayload::ExecutionFinished(finished)) => {
                assert!(matches!(
                    finished.outcome,
                    ExecutionOutcome::Succeeded { .. }
                ));
                break;
            }
            _ => {}
        }
    }
    assert!(
        service.home.join("timer/fae/alice/state.json").exists(),
        "timer creation output: {tool_outputs:?}"
    );
    client
        .request(RequestPayload::SessionConversationClear(Empty {}))
        .await
        .unwrap();
    client.close().await.unwrap();
    drop(client);
    config["agent"]["user_id"] = json!("bob");
    config["agent"]["session_id"] = json!("changed-default");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let state = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let state: Value = serde_json::from_slice(
                &std::fs::read(service.home.join("timer/fae/alice/state.json")).unwrap(),
            )
            .unwrap();
            if state["occurrences"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["status"] == "succeeded")
            {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let occurrence = state["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["status"] == "succeeded")
        .unwrap();
    assert_eq!(occurrence["owner"]["user_id"], "alice");
    let session = occurrence["session_id"].as_str().unwrap();
    assert_ne!(session, original);
    assert_ne!(session, "changed-default");
    let history = std::fs::read_to_string(
        service
            .home
            .join(format!("session/fae/alice/{session}.jsonl")),
    )
    .unwrap();
    assert!(history.contains("background timer input"));
    assert!(history.contains("mock answer"));
    assert!(
        !service
            .home
            .join(format!("session/fae/bob/{session}.jsonl"))
            .exists()
    );
    service.stop().await;
    model_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retained_context_and_target_partitions_follow_coordinator_facts() {
    let (base, mut requests, model_task) = model().await;
    let mut service = Service::start(Some(&base)).await;
    service.init("fae").await;
    service.init("other").await;
    let mut client = agent(&service).await;
    let sid = client.sid().to_string();
    let rid = RequestId::new("initial").unwrap();
    let ResponsePayload::SessionExecutionStart(accepted) = client
        .request_with_id(start_payload(client::text_content("hello")), rid.clone())
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(requests.recv().await.as_deref(), Some("hello"));
    assert!(matches!(
        finish(&mut client).await.1,
        ExecutionOutcome::Succeeded { .. }
    ));
    let ResponsePayload::SessionExecutionGet(result) = client
        .request(RequestPayload::SessionExecutionGet(ExecutionGetRequest {
            execution_id: Some(accepted.execution_id.clone()),
            input_request_id: Some(rid),
        }))
        .await
        .unwrap()
    else {
        panic!()
    };
    let execution = result.execution.unwrap();
    let input = result.acceptance.unwrap().input.unwrap();
    assert_eq!(input.status, fae_agent::InputStatus::Applied);
    assert!(execution.invocations.iter().any(|invocation| {
        invocation
            .model
            .as_ref()
            .is_some_and(|model| model.model == "mock" && model.provider == base)
    }));
    let ResponsePayload::SessionGet(summary) = client
        .request(RequestPayload::SessionGet(Empty {}))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(summary.context.memory.history_loaded);
    assert!(summary.context.memory.user_memories_loaded);
    assert_eq!(summary.context.memory.history_count, 2);
    client
        .request(RequestPayload::SessionConfigUpdate(ConfigUpdateRequest {
            target: Some(PlanTarget::Agent {
                agent_id: "other".into(),
            }),
            ..Default::default()
        }))
        .await
        .unwrap();
    let ResponsePayload::SessionConversationList(page) = client
        .request(RequestPayload::SessionConversationList(
            ConversationListRequest {
                offset: 0,
                limit: 100,
            },
        ))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(page.partition.target_id, "other");
    assert!(page.messages.is_empty());
    start_agent(&mut client, "other input").await;
    assert_eq!(requests.recv().await.as_deref(), Some("other input"));
    finish(&mut client).await;
    client
        .request(RequestPayload::SessionArchive(Empty {}))
        .await
        .unwrap();
    client
        .request(RequestPayload::SessionConversationClear(Empty {}))
        .await
        .unwrap();
    assert!(
        !service
            .home
            .join(format!(
                "session/other/{}/{sid}.jsonl",
                summary.user_info.user_id
            ))
            .exists()
    );
    client
        .request(RequestPayload::SessionExecutionGet(
            ExecutionGetRequest::default(),
        ))
        .await
        .unwrap();
    assert!(
        client
            .request(start_payload(client::text_content("archived")))
            .await
            .is_err()
    );
    service.stop().await;
    model_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_cache_is_shared_across_sessions_and_connections() {
    let mut service = Service::start(None).await;
    service.write_workflow("dedup", "result = 42");
    let first = workflow(&service, "dedup").await;
    let second = workflow(&service, "dedup").await;
    let mut a = client::connect(&service.home, &service.config, "/api/0.1.0/session")
        .await
        .unwrap();
    let mut b = client::connect(&service.home, &service.config, "/api/0.1.0/session")
        .await
        .unwrap();
    let request = ProtocolMessage::request(
        RequestId::new("shared-id").unwrap(),
        Some(SessionId::new(first.sid()).unwrap()),
        RequestPayload::SessionGet(Empty {}),
    )
    .unwrap();
    let original = exchange(&mut a, &request).await;
    assert_eq!(
        serde_json::to_value(exchange(&mut b, &request).await).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    let conflict = ProtocolMessage::request(
        RequestId::new("shared-id").unwrap(),
        Some(SessionId::new(second.sid()).unwrap()),
        RequestPayload::SessionArchive(Empty {}),
    )
    .unwrap();
    assert_eq!(
        exchange(&mut b, &conflict).await.code,
        ResponseCode::Conflict
    );
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_save_failure_releases_reservation_without_committing_defaults() {
    let mut service = Service::start(None).await;
    service.write_workflow("rollback", "result = 42");
    let mut client = workflow(&service, "rollback").await;
    let user = client.info.user_info.user_id.to_string();
    let path = service
        .home
        .join(format!("session/rollback/{user}/{}.json", client.sid()));
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let proposed = service.home.join("proposed");
    std::fs::create_dir(&proposed).unwrap();
    let mut socket = client::connect(&service.home, &service.config, "/api/0.1.0/session")
        .await
        .unwrap();
    let request = ProtocolMessage::request(
        RequestId::new("rollback").unwrap(),
        Some(SessionId::new(client.sid()).unwrap()),
        RequestPayload::SessionConfigUpdate(ConfigUpdateRequest {
            workspace: Some(proposed.to_string_lossy().into()),
            name: Some("must not commit".into()),
            ..Default::default()
        }),
    )
    .unwrap();
    let response = exchange(&mut socket, &request).await;
    assert_eq!(response.code, ResponseCode::StorageError);
    assert!(response.payload.is_null());
    let ResponsePayload::SessionGet(summary) = client
        .request(RequestPayload::SessionGet(Empty {}))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(summary.session_config.name, "test session");
    assert_eq!(
        summary.session_config.workspace.root,
        service.home.to_string_lossy()
    );
    start_workflow(&mut client).await;
    assert!(matches!(
        finish(&mut client).await.1,
        ExecutionOutcome::Succeeded { .. }
    ));
    service.stop().await;
}
