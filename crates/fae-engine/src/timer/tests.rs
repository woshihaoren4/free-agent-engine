use super::*;
use crate::{Engine, EngineBuilder};
use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use serde_json::json;

fn req(value: serde_json::Value) -> TimerRequest {
    serde_json::from_value(value).unwrap()
}
fn create(kind: &str, key: &str) -> TimerRequest {
    let mut s = json!({"kind":kind,"cron":"* * * * * *","timezone":"UTC"});
    if kind == "repeat" {
        s["max_fires"] = json!(3);
    }
    req(
        json!({"operation":"create","idempotency_key":key,"name":"test","input":"timer input","schedule":s}),
    )
}
fn query(id: &str) -> TimerRequest {
    req(json!({"operation":"query","id":id,"include_deleted":true,"include_occurrences":true}))
}
async fn context(home: &std::path::Path, user: &str, agent: &str) -> (Engine, Ctx) {
    let engine = EngineBuilder::new().build().await;
    let (_, session) =
        SingleAgentEnv::new_with_user_id(SingleAgentSource::AgentId(agent.into()), "test", user);
    let mut scope = ContextScope::root(user);
    scope.agent_id = Some(agent.into());
    scope.session = Some(ContextSession {
        session_id: "original".into(),
        session,
    });
    scope.timer_execution = Some(Arc::new(TimerExecutionBinding {
        owner: TimerOwner {
            agent_id: agent.into(),
            user_id: user.into(),
        },
        source: SingleAgentSource::AgentId(agent.into()),
        workspace: home.to_path_buf(),
    }));
    let ctx = engine.ctx_with_scope(scope).unwrap();
    (engine, ctx)
}
async fn fixture() -> (tempfile::TempDir, TimerRuntime, Engine, Ctx) {
    let home = tempfile::tempdir().unwrap();
    let runtime = TimerRuntime::with_home_dir(home.path()).await.unwrap();
    let (engine, ctx) = context(home.path(), "alice", "worker").await;
    (home, runtime, engine, ctx)
}

#[test]
fn cron_normalization_timezone_and_dst() {
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    for text in ["0 9 * * *", "0 0 9 * * *", "0 0 9 * * * 2026"] {
        let mut s = TimerSchedule::Once {
            cron: text.into(),
            timezone: "Asia/Shanghai".into(),
        };
        normalize(&mut s, now).unwrap();
        assert_eq!(
            timer::next(&s, now).unwrap().unwrap(),
            Utc.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap()
        );
    }
    for text in ["* *", "bad expression here * *", "* * * * * * * extra"] {
        assert!(
            timer::parse(&TimerSchedule::Loop {
                cron: text.into(),
                timezone: "UTC".into()
            })
            .is_err()
        );
    }
    assert!(
        timer::parse(&TimerSchedule::Loop {
            cron: "* * * * *".into(),
            timezone: "invalid".into()
        })
        .is_err()
    );
    let spring = TimerSchedule::Loop {
        cron: "0 30 2 * * *".into(),
        timezone: "America/New_York".into(),
    };
    let after = Utc.with_ymd_and_hms(2026, 3, 8, 0, 0, 0).unwrap();
    assert_eq!(
        timer::next(&spring, after).unwrap().unwrap(),
        Utc.with_ymd_and_hms(2026, 3, 9, 6, 30, 0).unwrap()
    );
    let fall = TimerSchedule::Loop {
        cron: "0 30 1 * * *".into(),
        timezone: "America/New_York".into(),
    };
    let early = Utc.with_ymd_and_hms(2026, 11, 1, 5, 30, 0).unwrap();
    assert_eq!(
        timer::next(&fall, early - ChronoDuration::seconds(1))
            .unwrap()
            .unwrap(),
        early
    );
    assert_eq!(
        timer::next(&fall, early).unwrap().unwrap(),
        Utc.with_ymd_and_hms(2026, 11, 2, 6, 30, 0).unwrap()
    );
    // 01:15 in the second fold is after 01:30 in the first fold.
    assert_eq!(
        timer::latest(&fall, Utc.with_ymd_and_hms(2026, 11, 1, 6, 15, 0).unwrap())
            .unwrap()
            .unwrap(),
        early
    );
}

#[tokio::test]
async fn once_persisted_before_event_and_duplicate_claim_suppression() {
    let (home, runtime, _engine, ctx) = fixture().await;
    let handle = runtime.handle();
    let created = handle.request(&ctx, create("once", "one")).await.unwrap();
    let t = &created.timers[0];
    let due = timer::timestamp(t.next_fire_at.as_ref().unwrap()).unwrap();
    let events = handle.poll(due).await.unwrap();
    assert_eq!(events.len(), 1);
    let disk = timer::load(home.path()).unwrap();
    let p = &disk.partitions[&("worker".into(), "alice".into())];
    assert_eq!(p.timers[0].accepted_count, 1);
    assert_eq!(p.timers[0].state, TimerState::Completed);
    assert_ne!(p.occurrences[0].session_id, "original");
    assert_eq!(events[0].session_id, p.occurrences[0].session_id);
    assert!(handle.claim(&events[0]).await.unwrap().is_some());
    assert!(handle.claim(&events[0]).await.unwrap().is_none());
    handle
        .acknowledge(
            &events[0].owner,
            &events[0].occurrence_id,
            TimerOccurrenceStatus::Failed,
            None,
            Some("test".into()),
        )
        .await
        .unwrap();
    assert!(
        handle
            .poll(due + ChronoDuration::days(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        handle.request(&ctx, query(&t.id)).await.unwrap().timers[0].accepted_count,
        1
    );
}

#[tokio::test]
async fn repeat_queues_only_latest_and_failure_consumes_count() {
    let (_home, runtime, _engine, ctx) = fixture().await;
    let h = runtime.handle();
    let t = h
        .request(&ctx, create("repeat", "repeat"))
        .await
        .unwrap()
        .timers
        .remove(0);
    let due = timer::timestamp(t.next_fire_at.as_ref().unwrap()).unwrap();
    let first = h.poll(due).await.unwrap().remove(0);
    h.claim(&first).await.unwrap().unwrap();
    h.acknowledge(
        &first.owner,
        &first.occurrence_id,
        TimerOccurrenceStatus::Running,
        Some("round".into()),
        None,
    )
    .await
    .unwrap();
    assert!(
        h.poll(due + ChronoDuration::seconds(30))
            .await
            .unwrap()
            .is_empty()
    );
    let state = h.request(&ctx, query(&t.id)).await.unwrap();
    assert_eq!(state.timers[0].accepted_count, 1);
    assert_eq!(
        timer::timestamp(state.timers[0].queued_at.as_ref().unwrap()).unwrap(),
        due + ChronoDuration::seconds(30)
    );
    h.acknowledge(
        &first.owner,
        &first.occurrence_id,
        TimerOccurrenceStatus::Failed,
        None,
        Some("failed".into()),
    )
    .await
    .unwrap();
    let second = h
        .poll(due + ChronoDuration::seconds(30))
        .await
        .unwrap()
        .remove(0);
    assert_ne!(second.session_id, first.session_id);
    h.claim(&second).await.unwrap().unwrap();
    h.acknowledge(
        &second.owner,
        &second.occurrence_id,
        TimerOccurrenceStatus::Cancelled,
        None,
        None,
    )
    .await
    .unwrap();
    let third = h
        .poll(due + ChronoDuration::seconds(31))
        .await
        .unwrap()
        .remove(0);
    h.claim(&third).await.unwrap().unwrap();
    h.acknowledge(
        &third.owner,
        &third.occurrence_id,
        TimerOccurrenceStatus::Succeeded,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        h.poll(due + ChronoDuration::days(1))
            .await
            .unwrap()
            .is_empty()
    );
    let state = h.request(&ctx, query(&t.id)).await.unwrap();
    assert_eq!(state.timers[0].accepted_count, 3);
    assert_eq!(state.timers[0].state, TimerState::Completed);
}

#[tokio::test]
async fn crud_idempotency_revision_and_owner_isolation() {
    let (home, runtime, _engine, ctx) = fixture().await;
    let h = runtime.handle();
    let t = h
        .request(&ctx, create("loop", "same"))
        .await
        .unwrap()
        .timers
        .remove(0);
    assert_eq!(
        h.request(&ctx, create("loop", "same"))
            .await
            .unwrap()
            .timers[0]
            .id,
        t.id
    );
    assert!(
        h.request(&ctx, create("once", "same"))
            .await
            .unwrap_err()
            .to_string()
            .contains("idempotency_conflict")
    );
    let (other_engine, other) = context(home.path(), "bob", "worker").await;
    assert!(
        h.request(&other, query(&t.id))
            .await
            .unwrap_err()
            .to_string()
            .contains("timer_not_found")
    );
    let paused = h
        .request(
            &ctx,
            req(json!({"operation":"update","id":t.id,"expected_revision":1,"enabled":false})),
        )
        .await
        .unwrap()
        .timers
        .remove(0);
    assert_eq!(paused.state, TimerState::Paused);
    assert!(
        h.request(
            &ctx,
            req(json!({"operation":"update","id":t.id,"expected_revision":1,"name":"stale"}))
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("revision_conflict")
    );
    assert!(
        h.poll(Utc::now() + ChronoDuration::days(1))
            .await
            .unwrap()
            .is_empty()
    );
    let resumed = h
        .request(
            &ctx,
            req(json!({"operation":"update","id":t.id,"expected_revision":2,"enabled":true})),
        )
        .await
        .unwrap()
        .timers
        .remove(0);
    assert_eq!(resumed.accepted_count, 0);
    let due = timer::timestamp(resumed.next_fire_at.as_ref().unwrap()).unwrap();
    let event = h.poll(due).await.unwrap().remove(0);
    h.request(
        &ctx,
        req(json!({"operation":"delete","id":t.id,"expected_revision":3})),
    )
    .await
    .unwrap();
    assert!(h.claim(&event).await.unwrap().is_none());
    let deleted = h.request(&ctx, query(&t.id)).await.unwrap();
    assert_eq!(
        deleted.occurrences[0].status,
        TimerOccurrenceStatus::Suppressed
    );
    assert_eq!(
        h.request(
            &ctx,
            req(json!({"operation":"delete","id":t.id,"expected_revision":1}))
        )
        .await
        .unwrap()
        .timers[0]
            .state,
        TimerState::Deleted
    );
    other_engine.exit().await.unwrap();
}

#[tokio::test]
async fn restart_pending_replays_same_session_running_interrupts_and_lock_excludes_writers() {
    let (home, runtime, _engine, ctx) = fixture().await;
    assert!(TimerRuntime::with_home_dir(home.path()).await.is_err());
    let t = runtime
        .handle()
        .request(&ctx, create("once", "restart"))
        .await
        .unwrap()
        .timers
        .remove(0);
    let due = timer::timestamp(t.next_fire_at.as_ref().unwrap()).unwrap();
    let first = runtime.handle().poll(due).await.unwrap().remove(0);
    drop(runtime);
    let runtime = TimerRuntime::with_home_dir(home.path()).await.unwrap();
    let replay = runtime.handle().poll(due).await.unwrap().remove(0);
    assert_eq!(replay.occurrence_id, first.occurrence_id);
    assert_eq!(replay.session_id, first.session_id);
    runtime.handle().claim(&replay).await.unwrap().unwrap();
    runtime
        .handle()
        .acknowledge(
            &replay.owner,
            &replay.occurrence_id,
            TimerOccurrenceStatus::Running,
            None,
            None,
        )
        .await
        .unwrap();
    drop(runtime);
    let runtime = TimerRuntime::with_home_dir(home.path()).await.unwrap();
    assert!(runtime.handle().poll(due).await.unwrap().is_empty());
    assert_eq!(
        runtime
            .handle()
            .request(&ctx, query(&t.id))
            .await
            .unwrap()
            .occurrences[0]
            .status,
        TimerOccurrenceStatus::Interrupted
    );
}

#[tokio::test]
async fn skip_once_expires_and_coalesce_bounds_offline_work() {
    let (_home, runtime, _engine, ctx) = fixture().await;
    let h = runtime.handle();
    let t = h.request(&ctx, req(json!({"operation":"create","idempotency_key":"skip","name":"skip","input":"test","schedule":{"kind":"once","cron":"* * * * * *"},"misfire_policy":"skip"}))).await.unwrap().timers.remove(0);
    let now = timer::timestamp(t.next_fire_at.as_ref().unwrap()).unwrap() + ChronoDuration::days(1);
    assert!(h.poll(now).await.unwrap().is_empty());
    let skipped = h.request(&ctx, query(&t.id)).await.unwrap();
    assert_eq!(skipped.timers[0].accepted_count, 0);
    assert_eq!(
        skipped.timers[0].completion_reason.as_deref(),
        Some("expired_skipped")
    );
    let loop_timer = h
        .request(&ctx, create("loop", "offline"))
        .await
        .unwrap()
        .timers
        .remove(0);
    assert_eq!(h.poll(now).await.unwrap().len(), 1);
    assert_eq!(
        h.request(&ctx, query(&loop_timer.id)).await.unwrap().timers[0].accepted_count,
        1
    );
}

#[tokio::test]
async fn input_snapshot_survives_update_and_clock_rollback() {
    let (_home, runtime, _engine, ctx) = fixture().await;
    let h = runtime.handle();
    let t = h
        .request(&ctx, create("loop", "snapshot"))
        .await
        .unwrap()
        .timers
        .remove(0);
    let due = timer::timestamp(t.next_fire_at.as_ref().unwrap()).unwrap();
    let first = h.poll(due).await.unwrap().remove(0);
    h.request(
        &ctx,
        req(json!({"operation":"update","id":t.id,"expected_revision":1,"input":"changed"})),
    )
    .await
    .unwrap();
    let o = h.claim(&first).await.unwrap().unwrap();
    assert_eq!(o.input, "timer input");
    h.acknowledge(
        &o.owner,
        &o.occurrence_id,
        TimerOccurrenceStatus::Succeeded,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        h.poll(due - ChronoDuration::seconds(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(h.poll(due).await.unwrap().is_empty());
    let second = h
        .poll(due + ChronoDuration::seconds(1))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(h.claim(&second).await.unwrap().unwrap().input, "changed");
}

#[tokio::test]
async fn storage_error_does_not_report_success_or_emit_and_corrupt_partition_isolated() {
    let (home, runtime, _engine, ctx) = fixture().await;
    std::fs::write(home.path().join("timer"), "block directory creation").unwrap();
    assert!(
        runtime
            .handle()
            .request(&ctx, create("once", "io"))
            .await
            .is_err()
    );
    assert!(
        runtime
            .handle()
            .poll(Utc::now() + ChronoDuration::days(1))
            .await
            .unwrap()
            .is_empty()
    );
    drop(runtime);
    std::fs::remove_file(home.path().join("timer")).unwrap();
    std::fs::create_dir_all(home.path().join("timer/worker/alice")).unwrap();
    let path = home.path().join("timer/worker/alice/state.json");
    std::fs::write(&path, "{bad json").unwrap();
    let runtime = TimerRuntime::with_home_dir(home.path()).await.unwrap();
    assert!(
        runtime
            .handle()
            .request(&ctx, create("once", "corrupt"))
            .await
            .unwrap_err()
            .to_string()
            .contains("timer_store_corrupt")
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{bad json");
    drop(runtime);
    std::fs::write(path, r#"{"schema_version":999}"#).unwrap();
    assert!(
        TimerRuntime::with_home_dir(home.path())
            .await
            .unwrap_err()
            .to_string()
            .contains("unsupported timer schema")
    );
}

#[tokio::test]
async fn scoped_subagent_and_task_binding_preserves_user_and_changes_source() {
    let (home, runtime, _engine, ctx) = fixture().await;
    let child = ctx
        .fork(ContextChildSpec {
            execution: true,
            agent_id: Some("child".into()),
            session: ctx.scope().unwrap().session.clone(),
            timer_source: Some(SingleAgentSource::AgentId("child".into())),
            ..Default::default()
        })
        .unwrap();
    let task = child.fork(ContextChildSpec::default()).unwrap();
    let t = runtime
        .handle()
        .request(&task, create("once", "child"))
        .await
        .unwrap()
        .timers
        .remove(0);
    assert_eq!(t.owner.user_id, "alice");
    assert_eq!(t.owner.agent_id, "child");
    assert_eq!(t.execution_binding.workspace, home.path());
    assert!(matches!(t.execution_binding.source, SingleAgentSource::AgentId(id) if id == "child"));
    for field in ["user_id", "agent_id", "session_id", "source", "workspace"] {
        let mut args = serde_json::to_value(create("once", "spoof")).unwrap();
        args[field] = json!("spoof");
        assert!(serde_json::from_value::<TimerRequest>(args).is_err());
    }
    let unbound = ctx.get_engine().context();
    assert!(
        runtime
            .handle()
            .request(&unbound, create("once", "unbound"))
            .await
            .is_err()
    );
    let workflow = _engine.ctx_with_scope(ContextScope::root("workflow-user")).unwrap();
    let (_, session) = SingleAgentEnv::new_with_user_id(SingleAgentSource::AgentId("workflow-agent".into()), "workflow input", "workflow-user");
    let agent = workflow.fork(ContextChildSpec {
        execution: true,
        agent_id: Some("workflow-agent".into()),
        session: Some(ContextSession { session_id: "workflow-child".into(), session }),
        timer_source: Some(SingleAgentSource::AgentId("workflow-agent".into())),
        timer_workspace: Some(home.path().to_path_buf()),
        ..Default::default()
    }).unwrap();
    let timer = runtime.handle().request(&agent, create("once", "workflow-child")).await.unwrap().timers.remove(0);
    assert_eq!(timer.owner.agent_id, "workflow-agent");
    assert_eq!(timer.owner.user_id, "workflow-user");
}

#[tokio::test]
async fn client_exit_keeps_scheduler_and_watch_has_one_consumer() {
    let (_home, runtime, _engine, ctx) = fixture().await;
    let receiver = runtime.watch().await.unwrap();
    assert!(runtime.watch().await.is_err());
    let client = runtime.handle().client();
    assert!(RuntimeSelectExec::watch(&client).await.is_err());
    RuntimeSelectExec::exit(&client).await.unwrap();
    let t = runtime
        .handle()
        .request(&ctx, create("once", "live"))
        .await
        .unwrap()
        .timers
        .remove(0);
    let event = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.from_rt_id, "timer");
    let EventType::Any(id, data) = event.event_type else {
        panic!()
    };
    assert_eq!(id, "timer");
    assert_eq!(data.downcast::<TimerFired>().unwrap().timer_id, t.id);
    runtime.exit().await.unwrap();
}

#[tokio::test]
async fn default_tool_routes_typed_timer_management_through_engine() {
    let (home, runtime, _engine, ctx) = fixture().await;
    let mut builder = EngineBuilder::new();
    builder.add_runtime(runtime.handle().client());
    let mut tools = crate::ToolsRuntime::new();
    tools.add_tool(Box::new(crate::DefaultTools::with_host_dir(home.path())));
    builder.add_runtime(tools);
    let engine = builder.build().await;
    let scope = (*ctx.scope().unwrap()).clone();
    let ctx = engine.ctx_with_scope(scope).unwrap();
    let response = engine
        .rt()
        .exec::<_, ToolResponse>(TaskReq {
            ctx,
            meta: TaskMeta {
                ty: TaskType::Tool,
                ..Default::default()
            },
            req: ToolRequest::new(
                "timer".into(),
                serde_json::to_string(&create("once", "tool")).unwrap(),
            ),
        })
        .await
        .unwrap();
    let mut response = response.resp;
    let result = response.next().await.unwrap();
    let ToolRespItem::Completed(result) = result else {
        panic!("unexpected timer result: {result:?}")
    };
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["timers"][0]["owner"]["user_id"], "alice", "{result}");
    engine.exit().await.unwrap();
}

#[tokio::test]
async fn failed_occurrence_commit_keeps_count_and_outbox_unchanged() {
    let (home, runtime, _engine, ctx) = fixture().await;
    let h = runtime.handle();
    let t = h
        .request(&ctx, create("once", "failed-accept"))
        .await
        .unwrap()
        .timers
        .remove(0);
    let due = timer::timestamp(t.next_fire_at.as_ref().unwrap()).unwrap();
    let root = home.path().join("timer");
    let backup = home.path().join("timer-backup");
    std::fs::rename(&root, &backup).unwrap();
    std::fs::write(&root, "cannot create partition directory").unwrap();
    assert!(h.poll(due).await.is_err());
    let state = h.request(&ctx, query(&t.id)).await.unwrap();
    assert_eq!(state.timers[0].accepted_count, 0);
    assert!(state.occurrences.is_empty());
    std::fs::remove_file(&root).unwrap();
    std::fs::rename(&backup, &root).unwrap();
    assert_eq!(h.poll(due).await.unwrap().len(), 1);
}

#[tokio::test]
async fn full_pending_queue_defers_acceptance_without_consuming_count() {
    let (_home, runtime, _engine, ctx) = fixture().await;
    let h = runtime.handle();
    let first_timer = h
        .request(&ctx, create("once", "fill"))
        .await
        .unwrap()
        .timers
        .remove(0);
    let due = timer::timestamp(first_timer.next_fire_at.as_ref().unwrap()).unwrap();
    h.poll(due).await.unwrap();
    {
        let mut store = h.0.store.lock().await;
        let p = store
            .partitions
            .get_mut(&("worker".into(), "alice".into()))
            .unwrap();
        let template = p.occurrences[0].clone();
        for _ in 1..TIMER_PENDING_LIMIT {
            let mut o = template.clone();
            o.occurrence_id = wd_tools::uuid::v4();
            o.session_id = wd_tools::uuid::v4();
            p.occurrences.push(o);
        }
    }
    let t = h
        .request(&ctx, create("once", "wait"))
        .await
        .unwrap()
        .timers
        .remove(0);
    let due = timer::timestamp(t.next_fire_at.as_ref().unwrap()).unwrap();
    let events = h.poll(due + ChronoDuration::days(1)).await.unwrap();
    assert_eq!(events.len(), TIMER_PENDING_LIMIT);
    let state = h.request(&ctx, query(&t.id)).await.unwrap();
    assert_eq!(state.timers[0].accepted_count, 0);
    assert!(state.timers[0].queued_at.is_some());
    let first = &events[0];
    h.claim(first).await.unwrap().unwrap();
    h.acknowledge(
        &first.owner,
        &first.occurrence_id,
        TimerOccurrenceStatus::Failed,
        None,
        None,
    )
    .await
    .unwrap();
    let events = h.poll(due + ChronoDuration::days(2)).await.unwrap();
    assert_eq!(events.len(), TIMER_PENDING_LIMIT);
    let new = events.iter().find(|e| e.timer_id == t.id).unwrap();
    let snapshot = h.claim(new).await.unwrap().unwrap();
    assert_eq!(
        timer::timestamp(&snapshot.scheduled_at).unwrap(),
        due + ChronoDuration::days(2)
    );
    assert_eq!(
        h.request(&ctx, query(&t.id)).await.unwrap().timers[0].accepted_count,
        1
    );
}
