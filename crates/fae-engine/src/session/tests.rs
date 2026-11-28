use super::SessionCoordinator;
use crate::{Engine, EngineBuilder, PlanRuntime};
use fae_agent::{protocol::*, *};
use std::sync::Arc;

fn data() -> ContextData {
    ContextData {
        user_info: UserInfo {
            user_id: UserId::new("alice").unwrap(),
            name: None,
        },
        session_config: SessionConfig {
            session_id: SessionId::new("chat").unwrap(),
            name: "test".into(),
            lifecycle: SessionLifecycle::Open,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            target: PlanTarget::Agent {
                agent_id: "agent".into(),
            },
            workspace: WorkspaceBinding {
                root: "/tmp".into(),
                project_files: vec![],
            },
            options: ExecutionOptions::default(),
            channels: vec![],
        },
        memory_view: MemoryView::default(),
        latest_execution_view: None,
    }
}

fn settings() -> ExecutionSettings {
    ExecutionSettings {
        target: data().session_config.target,
        workspace: "/tmp".into(),
        options: ExecutionOptions::default(),
        definitions: vec![],
    }
}

fn success() -> ExecutionOutcome {
    ExecutionOutcome::Succeeded {
        result: vec![ContentBlock::Json {
            value: serde_json::json!({"done": true}),
        }],
    }
}

async fn coordinator() -> Arc<SessionCoordinator> {
    SessionCoordinator::new(EngineBuilder::new().build().await, data()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn atomic_start_and_cleanup_barrier_isolate_late_reports() {
    let session = coordinator().await;
    let (a, b) = tokio::join!(async { session.prepare(settings()) }, async {
        session.prepare(settings())
    },);
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let execution = a.or(b).unwrap();
    session.running(&execution.execution_id).unwrap();
    let child = execution.ctx.fork(ContextChildSpec::default()).unwrap();
    child
        .invocation_started(&TaskMeta {
            id: "task".into(),
            plan_run_id: "plan".into(),
            executor: "runtime".into(),
            ..Default::default()
        })
        .unwrap();
    session
        .finishing(&execution.execution_id, success())
        .unwrap();
    session
        .cleaned(&execution.execution_id, &execution.invocation_id)
        .unwrap();
    assert_eq!(
        session.execution(None).unwrap().status,
        ExecutionStatus::Finishing
    );
    assert!(session.prepare(settings()).is_err());
    child.set_state(ContextState::Running).unwrap();
    child.set_state(ContextState::succeeded(())).unwrap();
    child.invocation_finished().unwrap();
    assert_eq!(
        session.execution(None).unwrap().status,
        ExecutionStatus::Succeeded
    );
    assert_eq!(
        session.execution(None).unwrap().outcome,
        session.execution(None).unwrap().outcome
    );
    let next = session.prepare(settings()).unwrap();
    assert_ne!(next.execution_id, execution.execution_id);
    assert!(
        session
            .finishing(&execution.execution_id, success())
            .is_err()
    );
    assert!(
        session
            .stop(&execution.execution_id, "late".into())
            .is_err()
    );
    assert_eq!(
        session.execution(None).unwrap().status,
        ExecutionStatus::Preparing
    );
}

#[tokio::test]
async fn cancellation_is_a_signal_and_accepted_inputs_get_one_decision() {
    let session = coordinator().await;
    let mut events = session.subscribe();
    let execution = session.prepare(settings()).unwrap();
    session.running(&execution.execution_id).unwrap();
    execution.ctx.set_state(ContextState::Running).unwrap();
    let child = execution.ctx.fork(ContextChildSpec::default()).unwrap();
    let request = RequestId::new("steer").unwrap();
    session
        .steer(
            &execution.execution_id,
            request,
            vec![ContentBlock::Text {
                text: "more".into(),
            }],
        )
        .unwrap();
    assert!(
        session
            .stop(&execution.execution_id, "stop".into())
            .unwrap()
    );
    assert!(!execution.ctx.state().current().is_terminal());
    assert!(child.cancel_requested());
    assert_eq!(&*child.cancellation().await, "stop");
    assert!(
        !session
            .stop(&execution.execution_id, "again".into())
            .unwrap()
    );
    assert!(session.take_inputs(&execution.execution_id).is_err());
    session
        .finishing(&execution.execution_id, success())
        .unwrap();
    assert!(session.read_summary().active_execution.is_some());
    session
        .cleaned(&execution.execution_id, &execution.invocation_id)
        .unwrap();
    let view = session.execution(None).unwrap();
    assert_eq!(view.status, ExecutionStatus::Cancelled);
    assert_eq!(view.inputs[0].status, InputStatus::NotApplied);
    assert_eq!(
        view.outcome,
        Some(ExecutionOutcome::Cancelled {
            reason: "stop".into()
        })
    );
    assert!(session.read_summary().active_execution.is_none());
    let mut finished = 0;
    let mut decided = 0;
    while let Ok(event) = events.try_recv() {
        match event {
            SessionControlEvent::Finished { .. } => finished += 1,
            SessionControlEvent::InputProcessed(_) => decided += 1,
            _ => {}
        }
    }
    assert_eq!((finished, decided), (1, 1));
    assert!(
        session
            .finishing(&execution.execution_id, success())
            .is_err()
    );
}

#[derive(Debug)]
struct TestEnv;
#[derive(Debug)]
struct Builder;
#[derive(Debug)]
struct TestPlan {
    ctx: Ctx,
}

#[async_trait::async_trait]
impl PlanBuilderWithEnv<TestEnv> for Builder {
    async fn build(&self, _rt: RT, ctx: Ctx, _env: TestEnv) -> anyhow::Result<Box<dyn Plan>> {
        Ok(Box::new(TestPlan { ctx }))
    }
}
#[async_trait::async_trait]
impl Plan for TestPlan {
    fn id(&self) -> &str {
        "plan"
    }
    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        self.ctx
            .set_state(ContextState::succeeded(serde_json::json!({"result": 42})))?;
        Ok(PlanNext::End)
    }
    async fn next(&mut self, _result: TaskResponse) -> anyhow::Result<PlanNext> {
        Ok(PlanNext::End)
    }
    async fn abort(&mut self, _code: i32, _error: String) {}
}

#[tokio::test]
async fn engine_execution_retains_public_result_across_rounds() {
    let mut builder = EngineBuilder::new();
    builder.add_runtime(PlanRuntime::new());
    builder.add_plan_builder_with_env_box(Box::new(Builder));
    let engine: Engine = builder.build().await;
    let session = SessionCoordinator::new(engine.clone(), data()).unwrap();
    for _ in 0..2 {
        let execution = session.prepare(settings()).unwrap();
        crate::execute_session(&engine, execution, TestEnv)
            .await
            .unwrap();
        let result = session.execution(None).unwrap();
        assert_eq!(result.status, ExecutionStatus::Succeeded);
        assert_eq!(result.outcome, session.execution(None).unwrap().outcome);
        assert_eq!(result.invocations[0].status, InvocationStatus::Succeeded);
        assert_eq!(result.invocations[0].plan_id.as_str(), "plan");
        assert!(result.invocations[0].task_id.is_some());
        assert_eq!(
            result.invocations[0].runtime_id.as_deref(),
            Some(PlanRuntime::ID)
        );
    }
}

#[tokio::test]
async fn definitions_are_fixed_within_execution_and_reloaded_for_next_execution() {
    let session = coordinator().await;
    let execution = session.prepare(settings()).unwrap();
    let target = settings().target;
    let first: serde_json::Value = execution
        .ctx
        .definition(
            "agent".into(),
            target.clone(),
            Some("definition.json".into()),
            async { Ok(serde_json::json!({"prompt": "first"})) },
        )
        .await
        .unwrap();
    let child = execution.ctx.fork(ContextChildSpec::default()).unwrap();
    let same: serde_json::Value = child
        .definition(
            "agent".into(),
            target.clone(),
            Some("definition.json".into()),
            async { anyhow::bail!("must not load the changed file") },
        )
        .await
        .unwrap();
    assert_eq!(first, same);
    let definition = session.execution(None).unwrap().effective.definitions[0].clone();
    assert_eq!(definition.content_fingerprint.len(), 64);
    session
        .finishing(&execution.execution_id, success())
        .unwrap();
    session
        .cleaned(&execution.execution_id, &execution.invocation_id)
        .unwrap();
    let next = session.prepare(settings()).unwrap();
    let changed: serde_json::Value = next
        .ctx
        .definition(
            "agent".into(),
            target,
            Some("definition.json".into()),
            async { Ok(serde_json::json!({"prompt": "second"})) },
        )
        .await
        .unwrap();
    assert_ne!(first, changed);
    assert_ne!(
        definition.content_fingerprint,
        session.execution(None).unwrap().effective.definitions[0].content_fingerprint,
    );
    assert!(
        execution
            .ctx
            .definition::<serde_json::Value, _>("late".into(), settings().target, None, async {
                Ok(serde_json::json!({}))
            },)
            .await
            .is_err()
    );
}

#[derive(Debug)]
struct FailEnv;
#[derive(Debug)]
struct FailBuilder;
#[async_trait::async_trait]
impl PlanBuilderWithEnv<FailEnv> for FailBuilder {
    async fn build(&self, _rt: RT, _ctx: Ctx, _env: FailEnv) -> anyhow::Result<Box<dyn Plan>> {
        anyhow::bail!("definition load failed")
    }
}

#[tokio::test]
async fn preparation_failure_retains_result_and_releases_slot() {
    let mut builder = EngineBuilder::new();
    builder.add_plan_builder_with_env_box(Box::new(FailBuilder));
    let engine = builder.build().await;
    let session = SessionCoordinator::new(engine.clone(), data()).unwrap();
    let execution = session.prepare(settings()).unwrap();
    assert!(
        crate::execute_session(&engine, execution, FailEnv)
            .await
            .is_err()
    );
    let view = session.execution(None).unwrap();
    assert_eq!(view.status, ExecutionStatus::Failed);
    assert!(matches!(
        view.outcome,
        Some(ExecutionOutcome::Failed { .. })
    ));
    assert!(session.prepare(settings()).is_ok());
}

#[derive(Debug)]
struct SlowEnv {
    started: Arc<tokio::sync::Notify>,
    abort_started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
#[derive(Debug)]
struct SlowBuilder;
#[derive(Debug)]
struct SlowPlan(SlowEnv);
#[async_trait::async_trait]
impl PlanBuilderWithEnv<SlowEnv> for SlowBuilder {
    async fn build(&self, _rt: RT, _ctx: Ctx, env: SlowEnv) -> anyhow::Result<Box<dyn Plan>> {
        Ok(Box::new(SlowPlan(env)))
    }
}
#[async_trait::async_trait]
impl Plan for SlowPlan {
    fn id(&self) -> &str {
        "slow"
    }
    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        self.0.started.notify_one();
        std::future::pending().await
    }
    async fn next(&mut self, _response: TaskResponse) -> anyhow::Result<PlanNext> {
        unreachable!()
    }
    async fn abort(&mut self, _code: i32, _message: String) {
        self.0.abort_started.notify_one();
        self.0.release.notified().await;
    }
}

#[tokio::test]
async fn stop_waits_for_actual_plan_abort_before_releasing_slot() {
    let mut builder = EngineBuilder::new();
    builder.add_runtime(PlanRuntime::new());
    builder.add_plan_builder_with_env_box(Box::new(SlowBuilder));
    let engine = builder.build().await;
    let session = SessionCoordinator::new(engine.clone(), data()).unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let abort_started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let execution = session.prepare(settings()).unwrap();
    let id = execution.execution_id.clone();
    let env = SlowEnv {
        started: started.clone(),
        abort_started: abort_started.clone(),
        release: release.clone(),
    };
    let job = tokio::spawn(async move { crate::execute_session(&engine, execution, env).await });
    started.notified().await;
    session.stop(&id, "stop".into()).unwrap();
    abort_started.notified().await;
    assert!(session.read_summary().active_execution.is_some());
    assert!(session.prepare(settings()).is_err());
    assert_eq!(
        session.execution(None).unwrap().status,
        ExecutionStatus::Stopping
    );
    release.notify_one();
    assert!(job.await.unwrap().is_err());
    assert_eq!(
        session.execution(None).unwrap().status,
        ExecutionStatus::Cancelled
    );
    assert!(session.prepare(settings()).is_ok());
}

#[derive(Debug)]
struct SlowRuntime {
    started: Arc<tokio::sync::Notify>,
    stopping: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl RuntimeSelectExec<(), (), (), ()> for SlowRuntime {
    fn id(&self) -> &str {
        "slow-runtime"
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Memory]
    }
    async fn exec(&self, task: TaskReq<()>) -> fae_agent::Result<TaskResp<()>> {
        self.started.notify_one();
        task.ctx.cancellation().await;
        self.stopping.notify_one();
        self.release.notified().await;
        Err(fae_agent::Error::ContextAborted)
    }
}

#[tokio::test]
async fn cancellation_keeps_runtime_future_alive_until_teardown_finishes() {
    let started = Arc::new(tokio::sync::Notify::new());
    let stopping = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut builder = EngineBuilder::new();
    builder.add_runtime(SlowRuntime {
        started: started.clone(),
        stopping: stopping.clone(),
        release: release.clone(),
    });
    let engine = builder.build().await;
    let session = SessionCoordinator::new(engine.clone(), data()).unwrap();
    let execution = session.prepare(settings()).unwrap();
    session.running(&execution.execution_id).unwrap();
    let task = TaskReq {
        ctx: execution.ctx.clone(),
        meta: TaskMeta {
            id: "slow-call".into(),
            ty: TaskType::Memory,
            ..Default::default()
        },
        req: (),
    };
    let job = tokio::spawn(async move { engine.rt().exec::<(), ()>(task).await });
    started.notified().await;
    session
        .stop(&execution.execution_id, "stop runtime".into())
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), stopping.notified())
        .await
        .unwrap();
    session
        .finishing(&execution.execution_id, success())
        .unwrap();
    session
        .cleaned(&execution.execution_id, &execution.invocation_id)
        .unwrap();
    assert!(!job.is_finished());
    assert!(session.read_summary().active_execution.is_some());
    assert!(session.prepare(settings()).is_err());
    release.notify_one();
    assert!(job.await.unwrap().is_err());
    assert!(session.read_summary().active_execution.is_none());
    assert_eq!(
        session.execution(None).unwrap().status,
        ExecutionStatus::Cancelled
    );
}

#[tokio::test]
async fn host_reservation_blocks_start_and_rolls_back_on_drop() {
    let session = coordinator().await;
    let initial = session.read_context();
    let reservation =
        SessionReservation::new(session.handle(), SessionOperation::Configure).unwrap();
    assert!(session.prepare(settings()).is_err());
    assert!(session.archive().is_err());
    assert!(
        session
            .commit_config(
                reservation.token(),
                ConfigUpdateRequest {
                    workspace: Some("relative".into()),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert_eq!(session.read_context(), initial);
    drop(reservation);
    let reservation =
        SessionReservation::new(session.handle(), SessionOperation::Configure).unwrap();
    session
        .commit_config(
            reservation.token(),
            ConfigUpdateRequest {
                name: Some("renamed".into()),
                workspace: Some("/tmp/other".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(session.read_context().session_config.name, "renamed");
    assert!(!session.read_summary().context.memory.project_files_loaded);
    assert_eq!(
        session.archive().unwrap().lifecycle,
        SessionLifecycle::Archived
    );
    assert_eq!(
        session.archive().unwrap().lifecycle,
        SessionLifecycle::Archived
    );
    assert!(session.prepare(settings()).is_err());
    let reservation = SessionReservation::new(session.handle(), SessionOperation::Clear).unwrap();
    session.commit_clear(reservation.token()).unwrap();
    assert_eq!(
        session.read_summary().session_config.lifecycle,
        SessionLifecycle::Archived
    );
}

#[tokio::test]
async fn bound_lark_target_cannot_change_and_child_history_keeps_its_partition() {
    let mut context = data();
    context.session_config.channels.push(ChannelBinding::Lark {
        app_id: "app".into(),
        chat_id: "chat".into(),
        thread_id: None,
    });
    let session = SessionCoordinator::new(EngineBuilder::new().build().await, context).unwrap();
    let reservation =
        SessionReservation::new(session.handle(), SessionOperation::Configure).unwrap();
    assert!(
        session
            .commit_config(
                reservation.token(),
                ConfigUpdateRequest {
                    target: Some(PlanTarget::Agent {
                        agent_id: "other".into()
                    }),
                    ..Default::default()
                }
            )
            .is_err()
    );
    drop(reservation);
    let execution = session.prepare(settings()).unwrap();
    session.running(&execution.execution_id).unwrap();
    let partition = ConversationPartition {
        target_id: "agent".into(),
        user_id: "alice".into(),
        session_id: SessionId::new("chat").unwrap(),
    };
    session
        .report(
            &execution.execution_id,
            ExecutionReport::HistoryLoaded {
                partition: partition.clone(),
                messages: vec![],
                offset: 0,
            },
        )
        .unwrap();
    assert!(session.read_summary().context.memory.history_loaded);
    let mut child = partition.clone();
    child.target_id = "child".into();
    let mut events = session.subscribe();
    session
        .report(
            &execution.execution_id,
            ExecutionReport::HistoryAdded {
                invocation_id: execution.invocation_id.clone(),
                partition: child.clone(),
                messages: vec![SessionMessage::assistant("child output")],
            },
        )
        .unwrap();
    assert!(session.read_context().memory_view.history.is_empty());
    assert!(
        matches!(events.try_recv().unwrap(), SessionControlEvent::ConversationAppended(e) if e.partition == child)
    );
    session
        .report(
            &execution.execution_id,
            ExecutionReport::HistoryAdded {
                invocation_id: execution.invocation_id,
                partition,
                messages: vec![SessionMessage::assistant("parent output")],
            },
        )
        .unwrap();
    assert_eq!(session.read_context().memory_view.history.len(), 1);
}

struct RacingSource {
    session: SessionHandle,
    reads: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl SessionDataSource for RacingSource {
    async fn read(&self, context: &ContextData) -> ControlResult<SessionStoredData> {
        if self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            // This mutation would deadlock if export held the coordinator lock across I/O.
            let reservation =
                SessionReservation::new(self.session.clone(), SessionOperation::Configure)?;
            self.session.commit_config(
                reservation.token(),
                ConfigUpdateRequest {
                    target: Some(PlanTarget::Agent {
                        agent_id: "changed".into(),
                    }),
                    ..Default::default()
                },
            )?;
        }
        let target = match &context.session_config.target {
            PlanTarget::Agent { agent_id } => agent_id,
            _ => unreachable!(),
        };
        Ok(SessionStoredData {
            history: vec![SessionMessage::assistant(target.clone())],
            user_memories: vec![],
            project_files: vec![],
        })
    }
}

#[tokio::test]
async fn export_rechecks_configuration_and_loads_before_first_start() {
    let session = coordinator().await;
    let source = RacingSource {
        session: session.handle(),
        reads: std::sync::atomic::AtomicUsize::new(0),
    };
    let snapshot = session.export_context(&source).await.unwrap();
    assert_eq!(source.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(snapshot.memory_view.history[0].content, "changed");
    assert_eq!(snapshot.memory_view.history_offset, 0);
    let summary = session.read_summary();
    assert!(summary.context.memory.history_loaded);
    assert!(summary.context.memory.user_memories_loaded);
    assert!(summary.context.memory.project_files_loaded);
    assert!(summary.active_execution.is_none());
    assert_eq!(
        serde_json::from_value::<ContextData>(serde_json::to_value(&snapshot).unwrap()).unwrap(),
        snapshot
    );
}

#[tokio::test]
async fn file_export_reads_original_partitions_and_does_not_create_files() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join(".fae")).unwrap();
    std::fs::write(workspace.path().join(".fae/project.md"), "project facts").unwrap();
    let mut context = data();
    context.session_config.workspace.root = workspace.path().to_string_lossy().into_owned();
    let session = SessionCoordinator::new(EngineBuilder::new().build().await, context).unwrap();
    let runtime = crate::SessionRuntime::with_host_dir(home.path());
    runtime
        .add("agent", "alice", "chat", &[SessionMessage::user("saved")])
        .await
        .unwrap();
    let snapshot = session
        .export_context(&super::SessionFileSource::new(home.path()))
        .await
        .unwrap();
    assert_eq!(snapshot.memory_view.history[0].content, "saved");
    assert_eq!(snapshot.session_config.workspace.project_files.len(), 4);
    assert_eq!(
        snapshot.session_config.workspace.project_files[3]
            .content
            .as_deref(),
        Some("project facts")
    );
    assert!(!home.path().join("memory").exists());
}

#[tokio::test]
async fn terminal_and_input_facts_survive_an_output_burst() {
    let session = coordinator().await;
    let mut events = session.subscribe();
    let execution = session.prepare(settings()).unwrap();
    let request_id = RequestId::new("initial-input").unwrap();
    session
        .report(
            &execution.execution_id,
            ExecutionReport::InitialInputAccepted(request_id.clone()),
        )
        .unwrap();
    session.running(&execution.execution_id).unwrap();
    execution.ctx.initial_input_applied().unwrap();
    for index in 0..5000 {
        execution
            .ctx
            .execution_output(
                index.to_string(),
                vec![ContentBlock::Text {
                    text: "delta".into(),
                }],
            )
            .unwrap();
    }
    session
        .finishing(&execution.execution_id, success())
        .unwrap();
    session
        .cleaned(&execution.execution_id, &execution.invocation_id)
        .unwrap();
    let mut outputs = 0;
    let mut finished = 0;
    let mut applied = 0;
    while let Ok(event) = events.try_recv() {
        match event {
            SessionControlEvent::Output { .. } => outputs += 1,
            SessionControlEvent::Finished { .. } => finished += 1,
            SessionControlEvent::InputProcessed(record) => {
                assert_eq!(record.request_id, request_id);
                assert_eq!(record.status, InputStatus::Applied);
                applied += 1;
            }
            _ => {}
        }
    }
    assert_eq!((outputs, finished, applied), (5000, 1, 1));
    let retained = session.execution(None).unwrap();
    assert_eq!(retained.inputs[0].status, InputStatus::Applied);
    assert_eq!(session.execution(None).unwrap(), retained);
    session.archive().unwrap();
    let reservation = SessionReservation::new(session.handle(), SessionOperation::Clear).unwrap();
    session.commit_clear(reservation.token()).unwrap();
    assert_eq!(session.execution(None).unwrap(), retained);
}

#[tokio::test]
async fn plugin_binding_uses_the_prepared_execution_identity() {
    let home = tempfile::tempdir().unwrap();
    let manager = Arc::new(fae_agent::plugin::PluginManager::new(home.path()));
    let engine = EngineBuilder::new().build().await.with_plugins(manager);
    let session = SessionCoordinator::new(engine, data()).unwrap();
    let execution = session.prepare(settings()).unwrap();
    assert_eq!(
        execution.ctx.plugin_execution().unwrap().execution_id,
        execution.execution_id
    );
    let child = execution.ctx.fork(ContextChildSpec::default()).unwrap();
    assert_eq!(
        child.plugin_execution().unwrap().execution_id,
        execution.execution_id
    );
}

#[tokio::test]
async fn cancellation_reaches_descendants_after_intermediate_scope_is_dropped() {
    let session = coordinator().await;
    let execution = session.prepare(settings()).unwrap();
    session.running(&execution.execution_id).unwrap();
    let intermediate = execution.ctx.fork(ContextChildSpec::default()).unwrap();
    let child = intermediate.fork(ContextChildSpec::default()).unwrap();
    drop(intermediate);
    session
        .stop(&execution.execution_id, "stop descendants".into())
        .unwrap();
    assert!(child.cancel_requested());
    assert_eq!(&*child.cancellation().await, "stop descendants");
}

#[tokio::test]
async fn execution_slot_waits_for_registered_resource_release() {
    let session = coordinator().await;
    let execution = session.prepare(settings()).unwrap();
    let resource = RequestId::new("plugin-call").unwrap();
    session
        .report(
            &execution.execution_id,
            ExecutionReport::ResourceRegistered {
                invocation_id: execution.invocation_id.clone(),
                resource_id: resource.clone(),
            },
        )
        .unwrap();
    session
        .finishing(&execution.execution_id, success())
        .unwrap();
    session
        .cleaned(&execution.execution_id, &execution.invocation_id)
        .unwrap();
    assert!(session.read_summary().active_execution.is_some());
    assert!(session.prepare(settings()).is_err());
    session
        .report(
            &execution.execution_id,
            ExecutionReport::ResourceReleased(resource.clone()),
        )
        .unwrap();
    assert_eq!(
        session.execution(None).unwrap().status,
        ExecutionStatus::Succeeded
    );
    let next = session.prepare(settings()).unwrap();
    assert!(
        session
            .report(
                &execution.execution_id,
                ExecutionReport::ResourceReleased(resource)
            )
            .is_err()
    );
    assert_eq!(
        session.execution(None).unwrap().execution_id,
        next.execution_id
    );
}
