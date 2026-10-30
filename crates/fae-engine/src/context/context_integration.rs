use crate::Engine;
use crate::*;
use async_openai::types::chat::{CreateChatCompletionRequest, CreateChatCompletionResponse};
use fae_agent::*;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use wd_tools::channel::{Channel, Receiver, Sender};

#[derive(Debug)]
struct ObservedCall {
    ctx: Ctx,
    identifier: Option<String>,
}

#[derive(Debug)]
struct MockModel {
    calls: Arc<Mutex<Vec<ObservedCall>>>,
    sender: Sender<Event>,
    receiver: Receiver<Event>,
}

impl MockModel {
    fn new(calls: Arc<Mutex<Vec<ObservedCall>>>) -> Self {
        let (sender, receiver) = Channel::new(32);
        Self {
            calls,
            sender,
            receiver,
        }
    }

    fn respond(&self, task: TaskReq<CreateChatCompletionRequest>) -> TaskResp<ModelResponse> {
        self.calls.lock().unwrap().push(ObservedCall {
            ctx: task.ctx.clone(),
            identifier: task.req.safety_identifier.clone(),
        });
        // Exercise the dedicated sub-agent invocation assembled by SingleAgentPlan.
        let scope = task.ctx.scope().unwrap();
        let has_tool_result = task.req.messages.iter().any(|message| {
            matches!(
                message,
                async_openai::types::chat::ChatCompletionRequestMessage::Tool(_)
            )
        });
        let planning_todos = task.req.tools.as_ref().is_some_and(|tools| {
            tools.iter().any(|tool| {
                matches!(tool, async_openai::types::chat::ChatCompletionTools::Function(tool)
                    if tool.function.name == "todo"
                        && tool.function.parameters.as_ref().is_some_and(|parameters|
                            parameters["properties"]["operation"]["enum"] == json!(["create"])))
            })
        });
        let message = if planning_todos && !has_tool_result {
            json!({"role": "assistant", "content": null, "tool_calls": [{
                "id": "plan-todos", "type": "function",
                "function": {"name": "todo", "arguments": "{\"operation\":\"create\",\"contents\":[\"write a poem\"]}"}
            }]})
        } else if scope.agent_id.as_deref() == Some("parent") && !has_tool_result {
            json!({"role": "assistant", "content": null, "tool_calls": [{
                "id": "delegate", "type": "function",
                "function": {"name": "agent", "arguments": "{\"agent_id\":\"worker\",\"input\":\"child task\",\"session_id\":\"override\"}"}
            }]})
        } else {
            json!({"role": "assistant", "content": "done"})
        };
        let response: CreateChatCompletionResponse = serde_json::from_value(json!({
            "id": "mock", "object": "chat.completion", "created": 0, "model": "mock",
            "choices": [{"index": 0, "message": message, "finish_reason": if has_tool_result || scope.agent_id.as_deref() != Some("parent") { "stop" } else { "tool_calls" }}]
        })).unwrap();
        TaskResp {
            ctx: task.ctx,
            meta: task.meta,
            resp: ModelResponse::Completed(response),
        }
    }
}

#[async_trait::async_trait]
impl RuntimeSelectExec<CreateChatCompletionRequest, ModelResponse, (), ()> for MockModel {
    fn id(&self) -> &str {
        "mock_model"
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Model]
    }
    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.receiver.clone())
    }
    async fn spawn(&self, task: TaskReq<CreateChatCompletionRequest>) -> fae_agent::Result<()> {
        self.sender
            .send(Event {
                from_rt_id: self.id().into(),
                event_type: EventType::TaskResult(self.respond(task).into_response()),
            })
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        Ok(())
    }
    async fn exec(
        &self,
        task: TaskReq<CreateChatCompletionRequest>,
    ) -> fae_agent::Result<TaskResp<ModelResponse>> {
        Ok(self.respond(task))
    }
}

async fn fixture() -> anyhow::Result<(
    Engine,
    PathBuf,
    FAEWorkflowMetadataLoader,
    Arc<Mutex<Vec<ObservedCall>>>,
)> {
    let home =
        std::env::temp_dir().join(format!("fae-context-integration-{}", wd_tools::uuid::v4()));
    tokio::fs::create_dir_all(home.join("agents")).await?;
    for name in ["worker", "parent", "poet"] {
        tokio::fs::write(home.join(format!("agents/{name}_config.json")), serde_json::to_vec(&json!({
            "agent": {"name": name, "desc": "test agent", "user_id": "config-user", "session_id": "configured"},
            "model": {"model": "mock"},
            "tools": if name == "poet" { vec!["todo"] } else { vec![] },
            "sub_agents": if name == "parent" { vec!["worker"] } else { vec![] }
        }))?).await?;
        tokio::fs::write(
            home.join(format!("agents/{name}_prompt.txt")),
            "Test prompt",
        )
        .await?;
    }
    let loader = FAEWorkflowMetadataLoader::with_home_dir(&home);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut builder = EngineBuilder::new();
    builder.add_runtime(PlanRuntime::new());
    builder.add_runtime(WorkflowRuntime::with_metadata_loader(loader.clone()));
    builder.add_runtime(MockModel::new(calls.clone()));
    builder.add_runtime(SessionRuntime::with_host_dir(&home));
    builder.add_runtime(UserMemoryRuntime::with_host_dir(&home));
    let mut tools = ToolsRuntime::new();
    tools.add_tool(Box::new(DefaultTools::with_host_dir(&home)));
    builder.add_runtime(tools);
    builder.add_plan_builder(SingleAgentPlanBuilder::with_home_dir(&home));
    builder.add_plan_builder(WorkflowPlanBuilder::new(loader.clone()));
    Ok((builder.build().await, home, loader, calls))
}

#[tokio::test]
async fn normal_todo_and_normal_turns_keep_task_contexts_independent() -> anyhow::Result<()> {
    let (engine, home, _, calls) = fixture().await?;
    let (env, session) = SingleAgentEnv::from_agent_id_with_user_id("poet", "hello", "alice");
    let first_ctx = engine.launch(env).await?;
    for (index, mode) in [None, Some("todo"), None].into_iter().enumerate() {
        if index > 0 {
            session.set_mode(mode).await?;
            session
                .call(SessionInput::NewChat("write a poem".into()))
                .await?;
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), session.result()).await??,
            json!("done")
        );
        loop {
            let event = tokio::time::timeout(Duration::from_secs(1), session.answer())
                .await??
                .ok_or_else(|| anyhow::anyhow!("session ended"))?;
            if event.is_terminal() {
                break;
            }
        }
    }
    assert_eq!(first_ctx.result::<Value>().await?, json!("done"));
    let observed = calls.lock().unwrap();
    assert_eq!(observed.len(), 4);
    for call in observed.iter() {
        let scope = call.ctx.scope()?;
        assert_eq!(scope.user.user_id, "alice");
        assert_eq!(scope.agent_id.as_deref(), Some("poet"));
        assert_eq!(scope.session.as_ref().unwrap().session_id, "configured");
        assert_eq!(call.identifier.as_deref(), Some("alice"));
        let stacks = call.ctx.stacks(ContextStackQuery {
            tree: true,
            ..Default::default()
        })?;
        assert!(stacks.iter().all(|entry| entry.scope_id == scope.scope_id));
        assert!(matches!(
            &*call.ctx.state().current(),
            ContextState::Succeeded { .. }
        ));
    }
    drop(observed);
    let todos: Value =
        serde_json::from_slice(&tokio::fs::read(home.join("session/poet/alice/todo.json")).await?)?;
    assert_eq!(todos["todos"]["1"]["completed"], json!(true));
    engine.exit().await?;
    tokio::fs::remove_dir_all(home).await?;
    Ok(())
}

fn delegation_workflows(loader: &FAEWorkflowMetadataLoader, dag: bool) -> anyhow::Result<()> {
    let mut child =
        WorkflowMetadataBuilder::new("child", "Delegate to agent through a prefixed tool");
    child.start("start", "agent")?;
    child.execute(
        "agent",
        WorkflowAction::Tool {
            tool_name: "default__agent".into(),
            arguments: json!({"agent_id": "worker", "input": "test", "session_id": "override"}),
        },
        "end",
    )?;
    child.end("end", Some(json!("{$agent}")))?;
    loader.add(child.build()?)?;
    let mut parent = WorkflowMetadataBuilder::new("root", "Test tool delegation");
    parent.start(
        "start",
        if dag {
            vec!["workflow", "agent"]
        } else {
            vec!["workflow"]
        },
    )?;
    parent.execute(
        "workflow",
        WorkflowAction::Tool {
            tool_name: "default__workflow".into(),
            arguments: json!({"workflow_id": "child", "input": {}}),
        },
        if dag { "end" } else { "agent" },
    )?;
    parent.execute(
        "agent",
        WorkflowAction::Tool {
            tool_name: "default__agent".into(),
            arguments: json!({"agent_id": "worker", "input": "test"}),
        },
        "end",
    )?;
    parent.end(
        "end",
        Some(json!({"agent": "{$agent}", "workflow": "{$workflow}"})),
    )?;
    loader.add(parent.build()?)?;
    Ok(())
}

#[tokio::test]
async fn sequential_and_dag_tool_delegation_isolates_concurrent_users() -> anyhow::Result<()> {
    for dag in [false, true] {
        let (engine, home, loader, calls) = fixture().await?;
        delegation_workflows(&loader, dag)?;
        let (alice, alice_session) = WorkflowEnv::new_with_user_id("root", Value::Null, "alice");
        let (bob, _) = WorkflowEnv::new_with_user_id("root", Value::Null, "bob");
        let (alice, bob) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(
                engine.invoke::<_, Value>(alice),
                engine.invoke::<_, Value>(bob)
            )
        })
        .await?;
        let (alice, alice_output) = alice?;
        let (bob, bob_output) = bob?;
        assert_eq!(alice_output, json!({"agent":"done", "workflow":"done"}));
        assert_eq!(bob_output, alice_output);
        assert_eq!(alice_session.result().await?, alice_output);
        assert_ne!(alice.scope()?.scope_id, bob.scope()?.scope_id);
        let observed = calls.lock().unwrap();
        assert_eq!(observed.len(), 4);
        for user in ["alice", "bob"] {
            let user_calls = observed
                .iter()
                .filter(|call| call.ctx.scope().unwrap().user.user_id == user)
                .collect::<Vec<_>>();
            assert_eq!(user_calls.len(), 2);
            assert!(
                user_calls
                    .iter()
                    .all(|call| call.identifier.as_deref() == Some(user))
            );
            assert!(
                user_calls
                    .iter()
                    .all(|call| call.ctx.scope().unwrap().agent_id.as_deref() == Some("worker"))
            );
            assert!(
                home.join(format!("session/worker/{user}/override.jsonl"))
                    .exists()
            );
            assert!(
                home.join(format!("session/worker/{user}/configured.jsonl"))
                    .exists()
            );
        }
        assert!(!home.join("session/worker/master").exists());
        assert!(!home.join("session/worker/config-user").exists());
        drop(observed);
        engine.exit().await?;
        tokio::fs::remove_dir_all(home).await?;
    }
    Ok(())
}

#[tokio::test]
async fn dedicated_agent_invocation_and_next_turn_preserve_session_and_identity()
-> anyhow::Result<()> {
    let (engine, home, _, calls) = fixture().await?;
    let (env, session) = SingleAgentEnv::from_agent_id_with_user_id("parent", "delegate", "alice");
    let ctx = engine.launch(env).await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), session.result()).await??,
        json!("done")
    );
    ctx.terminal().await?;
    assert_eq!(
        ctx.scope()?.session.as_ref().unwrap().session_id,
        "configured"
    );
    assert!(home.join("session/worker/alice/override.jsonl").exists());
    let mut forwarded_child = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), session.answer())
            .await??
            .ok_or_else(|| anyhow::anyhow!("session ended"))?;
        if event.agent_name.as_deref() == Some("worker") {
            forwarded_child = true;
        }
        if event.is_terminal() {
            break;
        }
    }
    assert!(forwarded_child);
    session
        .call(SessionInput::NewChat(SessionInputData::text("again")))
        .await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), session.result()).await??,
        json!("done")
    );
    let observed = calls.lock().unwrap();
    let parent_turns = observed
        .iter()
        .filter_map(|call| {
            let scope = call.ctx.scope().unwrap();
            (scope.agent_id.as_deref() == Some("parent")).then_some((
                scope.turn_id,
                scope.scope_id.clone(),
                scope.session.as_ref().unwrap().session_id.clone(),
            ))
        })
        .collect::<Vec<_>>();
    assert!(parent_turns.iter().any(|turn| turn.0 == Some(1)));
    assert!(parent_turns.iter().any(|turn| turn.0 == Some(2)));
    assert!(parent_turns.iter().all(|turn| turn.2 == "configured"));
    let first = parent_turns.iter().find(|turn| turn.0 == Some(1)).unwrap();
    let second = parent_turns.iter().find(|turn| turn.0 == Some(2)).unwrap();
    assert_ne!(first.1, second.1);
    assert!(matches!(
        &*ctx.state().current(),
        ContextState::Succeeded { .. }
    ));
    assert!(
        observed
            .iter()
            .all(|call| call.identifier.as_deref() == Some("alice"))
    );
    drop(observed);
    engine.exit().await?;
    tokio::fs::remove_dir_all(home).await?;
    Ok(())
}

#[derive(Debug)]
struct RejectEnd;
#[async_trait::async_trait]
impl WorkflowHookBuilder for RejectEnd {
    async fn build(&self) -> Arc<dyn WorkflowHook> {
        Arc::new(Self)
    }
}
#[async_trait::async_trait]
impl WorkflowHook for RejectEnd {
    async fn on_end(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        if matches!(ctx.phase, WorkflowHookPhase::After { .. }) {
            anyhow::bail!("end hook rejected");
        }
        Ok(())
    }
}

#[tokio::test]
async fn end_after_hook_failure_never_publishes_success() -> anyhow::Result<()> {
    for dag in [false, true] {
        let loader = FAEWorkflowMetadataLoader::new();
        let mut flow = WorkflowMetadataBuilder::new("hook", "Reject end hook");
        flow.start(
            "start",
            if dag {
                vec!["left", "right"]
            } else {
                vec!["end"]
            },
        )?;
        if dag {
            for node in ["left", "right"] {
                flow.execute(
                    node,
                    WorkflowAction::Tool {
                        tool_name: LIST_DIRECTORY.into(),
                        arguments: json!({"path": env!("CARGO_MANIFEST_DIR")}),
                    },
                    "end",
                )?;
            }
        }
        flow.end("end", Some(json!("done")))?;
        loader.add(flow.build()?)?;
        let mut workflow_builder = WorkflowPlanBuilder::new(loader);
        workflow_builder.add_hook(RejectEnd);
        let mut builder = EngineBuilder::new();
        builder.add_runtime(PlanRuntime::new());
        let mut tools = ToolsRuntime::new();
        tools.add_tool(Box::new(DefaultTools::default()));
        builder.add_runtime(tools);
        builder.add_plan_builder(workflow_builder);
        let engine = builder.build().await;
        let (env, session) = WorkflowEnv::new_with_user_id("hook", Value::Null, "alice");
        let ctx = engine.launch(env).await?;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), ctx.result::<Value>())
                .await?
                .unwrap_err()
                .to_string(),
            "end hook rejected"
        );
        assert_eq!(
            session.result().await.unwrap_err().to_string(),
            "end hook rejected"
        );
        loop {
            let event = session.answer().await?.unwrap();
            assert!(!matches!(
                event.event_data()?,
                SessionEventData::NodeCompleted { finished: true, .. }
            ));
            if event.is_terminal() {
                break;
            }
        }
        engine.exit().await?;
    }
    Ok(())
}

#[tokio::test]
async fn user_data_tools_and_requests_reject_missing_or_mismatched_scope() -> anyhow::Result<()> {
    let (engine, home, _, _) = fixture().await?;
    let null = Ctx::new(Arc::new(ContextNull));
    for tool in [AGENT, WORKFLOW, MEMORY_UPDATE, TODO] {
        let arguments = match tool {
            AGENT => json!({"agent_id":"worker", "input":"test"}),
            WORKFLOW => json!({"workflow_id":"child", "input":{}}),
            MEMORY_UPDATE => json!({"operation":"query"}),
            _ => json!({"operation":"query"}),
        };
        let mut task = TaskReq {
            ctx: null.clone(),
            meta: TaskMeta {
                ty: TaskType::Tool,
                ..Default::default()
            },
            req: ToolRequest::new(tool.into(), arguments.to_string()),
        }
        .into_request();
        assert!(
            Runtime::exec(&*engine.rt(), &mut task).await.is_err(),
            "{tool}"
        );
    }
    let ctx = engine.ctx_with_scope(ContextScope::root("alice"))?;
    let error = engine
        .rt()
        .exec::<_, ToolResponse>(TaskReq {
            ctx: ctx.clone(),
            meta: TaskMeta {
                ty: TaskType::Tool,
                ..Default::default()
            },
            req: ToolRequest::new(TODO.into(), json!({"operation":"query"}).to_string()),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("agent"));
    let error = engine
        .rt()
        .exec::<_, UserMemoryResponse>(TaskReq {
            ctx: ctx.clone(),
            meta: TaskMeta {
                ty: TaskType::Memory,
                ..Default::default()
            },
            req: UserMemoryRequest::Query {
                user_id: "bob".into(),
            },
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("user"));
    let error = engine
        .rt()
        .exec::<_, SessionResponse>(TaskReq {
            ctx,
            meta: TaskMeta {
                ty: TaskType::Session,
                ..Default::default()
            },
            req: SessionRequest::Add {
                agent_id: "worker".into(),
                user_id: "bob".into(),
                session_id: "override".into(),
                messages: vec![],
            },
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("user"));
    assert!(!home.join("session/worker/bob").exists());
    engine.exit().await?;
    tokio::fs::remove_dir_all(home).await?;
    Ok(())
}

#[derive(Debug)]
struct StreamingModel;
#[async_trait::async_trait]
impl RuntimeSelectExec<bool, ModelResponse, (), ()> for StreamingModel {
    fn id(&self) -> &str {
        "stream_model"
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Model]
    }
    async fn exec(&self, task: TaskReq<bool>) -> fae_agent::Result<TaskResp<ModelResponse>> {
        let stream: async_openai::types::chat::ChatCompletionResponseStream = if task.req {
            Box::pin(tokio_stream::iter(vec![Ok(serde_json::from_value(
                json!({
                    "id":"chunk", "object":"chat.completion.chunk", "created":0, "model":"mock",
                    "choices":[{"index":0, "delta":{"content":"hello"}, "finish_reason":null}]
                }),
            )
            .unwrap())]))
        } else {
            Box::pin(tokio_stream::pending())
        };
        Ok(TaskResp {
            ctx: task.ctx,
            meta: task.meta,
            resp: ModelResponse::Streaming(stream),
        })
    }
}

#[tokio::test]
async fn model_stream_terminal_state_follows_eof_drop_and_cancellation() -> anyhow::Result<()> {
    use tokio_stream::StreamExt;
    let mut builder = EngineBuilder::new();
    builder.add_runtime(StreamingModel);
    let engine = builder.build().await;
    let root = engine.ctx_with_scope(ContextScope::root("alice"))?;
    let response = engine
        .rt()
        .exec::<_, ModelResponse>(TaskReq {
            ctx: root.clone(),
            meta: TaskMeta {
                ty: TaskType::Model,
                ..Default::default()
            },
            req: true,
        })
        .await?;
    let task_ctx = response.ctx;
    let mut stream = response.resp.into_stream().unwrap();
    assert!(matches!(
        &*task_ctx.state().current(),
        ContextState::Running
    ));
    assert!(stream.next().await.unwrap().is_ok());
    assert!(matches!(
        &*task_ctx.state().current(),
        ContextState::Running
    ));
    assert!(stream.next().await.is_none());
    assert!(matches!(
        &*task_ctx.state().current(),
        ContextState::Succeeded { .. }
    ));
    assert!(!root.state().current().is_terminal());
    assert_eq!(
        task_ctx
            .stacks(ContextStackQuery {
                phase: Some(ContextStackPhase::Succeeded),
                ..Default::default()
            })?
            .len(),
        1
    );

    let response = engine
        .rt()
        .exec::<_, ModelResponse>(TaskReq {
            ctx: root.clone(),
            meta: TaskMeta {
                ty: TaskType::Model,
                ..Default::default()
            },
            req: false,
        })
        .await?;
    let dropped_ctx = response.ctx;
    drop(response.resp);
    assert!(dropped_ctx.state().current().is_cancelled());
    let response = engine
        .rt()
        .exec::<_, ModelResponse>(TaskReq {
            ctx: root.clone(),
            meta: TaskMeta {
                ty: TaskType::Model,
                ..Default::default()
            },
            req: false,
        })
        .await?;
    let cancelled_ctx = response.ctx;
    let mut stream = response.resp.into_stream().unwrap();
    root.set_state(ContextState::cancelled("stop stream"))?;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await?
            .is_none()
    );
    assert_eq!(
        cancelled_ctx.result::<()>().await.unwrap_err().to_string(),
        "stop stream"
    );
    engine.exit().await?;
    Ok(())
}

#[derive(Debug)]
struct StreamingTool {
    producer: Arc<Mutex<Option<ToolResponse>>>,
}
#[async_trait::async_trait]
impl RuntimeSelectExec<(), ToolResponse, (), ()> for StreamingTool {
    fn id(&self) -> &str {
        "stream_tool"
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Tool]
    }
    async fn exec(&self, task: TaskReq<()>) -> fae_agent::Result<TaskResp<ToolResponse>> {
        let response = ToolResponse::with_stream_chan();
        *self.producer.lock().unwrap() = Some(response.clone());
        Ok(TaskResp {
            ctx: task.ctx,
            meta: task.meta,
            resp: response,
        })
    }
}

#[tokio::test]
async fn tool_stream_dispatch_and_partial_output_do_not_finish_task() -> anyhow::Result<()> {
    let producer = Arc::new(Mutex::new(None));
    let mut builder = EngineBuilder::new();
    builder.add_runtime(StreamingTool {
        producer: producer.clone(),
    });
    let engine = builder.build().await;
    let root = engine.ctx_with_scope(ContextScope::root("alice"))?;
    let mut response = engine
        .rt()
        .exec::<_, ToolResponse>(TaskReq {
            ctx: root.clone(),
            meta: TaskMeta {
                ty: TaskType::Tool,
                ..Default::default()
            },
            req: (),
        })
        .await?;
    let output = producer.lock().unwrap().take().unwrap();
    assert!(matches!(
        &*response.ctx.state().current(),
        ContextState::Running
    ));
    output.streaming_push("partial".into()).await?;
    assert!(matches!(
        response.resp.next().await?,
        ToolRespItem::Streaming(_)
    ));
    assert!(matches!(
        &*response.ctx.state().current(),
        ContextState::Running
    ));
    output.completed_push("finished".into()).await?;
    assert!(matches!(
        response.resp.next().await?,
        ToolRespItem::Completed(_)
    ));
    assert!(matches!(
        &*response.ctx.state().current(),
        ContextState::Succeeded { .. }
    ));
    assert!(!root.state().current().is_terminal());
    let response = engine
        .rt()
        .exec::<_, ToolResponse>(TaskReq {
            ctx: root.clone(),
            meta: TaskMeta {
                ty: TaskType::Tool,
                ..Default::default()
            },
            req: (),
        })
        .await?;
    let task_ctx = response.ctx.clone();
    let cloned_response = response.resp.clone();
    drop(response.resp);
    assert!(!task_ctx.state().current().is_terminal());
    drop(cloned_response);
    assert!(task_ctx.state().current().is_cancelled());
    engine.exit().await?;
    Ok(())
}

#[derive(Debug)]
struct RetainedStreamRuntime<R> {
    ty: TaskType,
    response: fn() -> R,
    sender: Sender<Event>,
    receiver: Receiver<Event>,
    advanced: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl<R: std::fmt::Debug + Send + 'static> RuntimeSelectExec<(), R, (), ()>
    for RetainedStreamRuntime<R>
{
    fn id(&self) -> &str {
        "retained_stream"
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![self.ty.clone()]
    }
    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.receiver.clone())
    }
    async fn spawn(&self, task: TaskReq<()>) -> fae_agent::Result<()> {
        if task.meta.id == "hold" {
            self.advanced.notify_one();
            return Ok(());
        }
        self.sender
            .send(Event {
                from_rt_id: self.id().into(),
                event_type: EventType::TaskResult(
                    TaskResp {
                        ctx: task.ctx,
                        meta: task.meta,
                        resp: (self.response)(),
                    }
                    .into_response(),
                ),
            })
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        Ok(())
    }
}

#[derive(Debug)]
struct RetainedStreamPlan {
    ctx: Ctx,
    ty: TaskType,
    response: Arc<Mutex<Option<TaskResponse>>>,
}

impl RetainedStreamPlan {
    fn task(&self, id: &str) -> TaskRequest {
        TaskReq {
            ctx: self.ctx.clone(),
            meta: TaskMeta {
                id: id.into(),
                ty: self.ty.clone(),
                ..Default::default()
            },
            req: (),
        }
        .into_request()
    }
}

#[async_trait::async_trait]
impl Plan for RetainedStreamPlan {
    fn id(&self) -> &str {
        "retain-stream"
    }
    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        Ok(PlanNext::Tasks(vec![self.task("stream")]))
    }
    async fn next(&mut self, response: TaskResponse) -> anyhow::Result<PlanNext> {
        *self.response.lock().unwrap() = Some(response);
        Ok(PlanNext::Tasks(vec![self.task("hold")]))
    }
    async fn abort(&mut self, _: i32, _: String) {}
}

async fn retain_stream<R: std::fmt::Debug + Send + 'static>(
    ty: TaskType,
    response: fn() -> R,
) -> anyhow::Result<(Engine, Ctx, TaskResponse)> {
    let held = Arc::new(Mutex::new(None));
    let advanced = Arc::new(tokio::sync::Notify::new());
    let (sender, receiver) = Channel::new(16);
    let mut builder = EngineBuilder::new();
    builder.add_runtime(PlanRuntime::new());
    builder.add_runtime(RetainedStreamRuntime {
        ty: ty.clone(),
        response,
        sender,
        receiver,
        advanced: advanced.clone(),
    });
    let engine = builder.build().await;
    let root = engine.ctx_with_scope(ContextScope::root("alice"))?;
    let plan: Box<dyn Plan> = Box::new(RetainedStreamPlan {
        ctx: root.clone(),
        ty,
        response: held.clone(),
    });
    engine
        .rt()
        .spawn(TaskReq {
            ctx: root.clone(),
            meta: TaskMeta {
                ty: TaskType::Plan,
                ..Default::default()
            },
            req: plan,
        })
        .await?;
    // The next dispatch happens after PlanRuntime has returned from the callback.
    tokio::time::timeout(Duration::from_secs(2), advanced.notified()).await?;
    let response = held.lock().unwrap().take().unwrap();
    assert!(matches!(
        &*response.ctx.state().current(),
        ContextState::Running
    ));
    Ok((engine, root, response))
}

#[tokio::test]
async fn regression_plan_retained_tool_stream_finishes_only_on_consumption() -> anyhow::Result<()> {
    for outcome in ["completed", "cancelled", "dropped"] {
        let (engine, root, mut response) =
            retain_stream(TaskType::Tool, ToolResponse::with_stream_chan).await?;
        let response = TaskResp::<ToolResponse>::try_from_response(&mut response).unwrap();
        let task_ctx = response.ctx;
        let mut stream = response.resp;
        match outcome {
            "completed" => {
                let producer = stream.clone();
                producer.streaming_push("partial".into()).await?;
                assert!(matches!(stream.next().await?, ToolRespItem::Streaming(_)));
                assert!(matches!(
                    &*task_ctx.state().current(),
                    ContextState::Running
                ));
                producer.completed_push("done".into()).await?;
                assert!(matches!(stream.next().await?, ToolRespItem::Completed(_)));
                assert!(matches!(
                    &*task_ctx.state().current(),
                    ContextState::Succeeded { .. }
                ));
            }
            "cancelled" => {
                root.set_state(ContextState::cancelled("stop retained stream"))?;
                let error = tokio::time::timeout(Duration::from_secs(1), stream.next())
                    .await?
                    .unwrap_err();
                assert_eq!(error.to_string(), "stop retained stream");
                assert!(task_ctx.state().current().is_cancelled());
            }
            "dropped" => {
                drop(stream);
                assert!(task_ctx.state().current().is_cancelled());
            }
            _ => unreachable!(),
        }
        if !root.state().current().is_terminal() {
            root.set_state(ContextState::cancelled("finish test"))?;
        }
        engine.exit().await?;
    }
    Ok(())
}

#[tokio::test]
async fn regression_plan_retained_model_stream_finishes_only_on_consumption() -> anyhow::Result<()>
{
    use tokio_stream::StreamExt;
    fn response() -> ModelResponse {
        ModelResponse::Streaming(Box::pin(tokio_stream::iter(vec![Ok(
            serde_json::from_value(json!({
                "id":"chunk", "object":"chat.completion.chunk", "created":0, "model":"mock",
                "choices":[{"index":0, "delta":{"content":"hello"}, "finish_reason":null}]
            }))
            .unwrap(),
        )])))
    }
    for outcome in ["completed", "cancelled", "dropped"] {
        let (engine, root, mut result) = retain_stream(TaskType::Model, response).await?;
        let response = TaskResp::<ModelResponse>::try_from_response(&mut result).unwrap();
        let task_ctx = response.ctx;
        let mut stream = response.resp.into_stream().unwrap();
        match outcome {
            "completed" => {
                assert!(stream.next().await.unwrap().is_ok());
                assert!(matches!(
                    &*task_ctx.state().current(),
                    ContextState::Running
                ));
                assert!(stream.next().await.is_none());
                assert!(matches!(
                    &*task_ctx.state().current(),
                    ContextState::Succeeded { .. }
                ));
            }
            "cancelled" => {
                root.set_state(ContextState::cancelled("stop retained stream"))?;
                assert!(
                    tokio::time::timeout(Duration::from_secs(1), stream.next())
                        .await?
                        .is_none()
                );
                assert_eq!(
                    task_ctx.result::<()>().await.unwrap_err().to_string(),
                    "stop retained stream"
                );
            }
            "dropped" => {
                drop(stream);
                assert!(task_ctx.state().current().is_cancelled());
            }
            _ => unreachable!(),
        }
        if !root.state().current().is_terminal() {
            root.set_state(ContextState::cancelled("finish test"))?;
        }
        engine.exit().await?;
    }
    Ok(())
}

async fn assert_async_trace<R: Send + 'static>(
    engine: &Engine,
    ctx: &Ctx,
    ty: TaskType,
    req: R,
    terminal: ContextStackPhase,
) -> anyhow::Result<()> {
    let id = wd_tools::uuid::v4();
    let events = engine.rt().watch().await?;
    engine
        .rt()
        .spawn(TaskReq {
            ctx: ctx.clone(),
            meta: TaskMeta {
                id: id.clone(),
                plan_id: "trace-plan".into(),
                publisher: "trace-publisher".into(),
                ty: ty.clone(),
                ..Default::default()
            },
            req,
        })
        .await?;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv()).await??;
        let meta = match &event.event_type {
            EventType::TaskResult(response) => &response.meta,
            EventType::TaskError(error) => &error.meta,
            _ => continue,
        };
        if meta.id == id {
            assert_eq!(
                matches!(event.event_type, EventType::TaskResult(_)),
                terminal == ContextStackPhase::Succeeded
            );
            break;
        }
    }
    let entries = ctx.stacks(ContextStackQuery {
        tree: true,
        task_id: Some(id.clone()),
        ..Default::default()
    })?;
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.phase.clone())
            .collect::<Vec<_>>(),
        vec![
            ContextStackPhase::Dispatched,
            ContextStackPhase::Started,
            terminal
        ]
    );
    assert_eq!(entries[2].parent_entry_id, Some(entries[1].entry_id));
    assert!(entries[2].elapsed_ms.is_some());
    assert!(
        entries
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    for entry in entries {
        assert_eq!(entry.task_meta.id, id);
        assert_eq!(entry.task_meta.ty, ty);
        assert_eq!(entry.task_meta.plan_id, "trace-plan");
        assert_eq!(entry.task_meta.publisher, "trace-publisher");
        assert!(!entry.task_meta.executor.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn regression_async_runtimes_record_actual_start_and_terminal_link() -> anyhow::Result<()> {
    let home = std::env::temp_dir().join(format!("fae-async-trace-{}", wd_tools::uuid::v4()));
    tokio::fs::create_dir_all(home.join("mcp")).await?;
    tokio::fs::write(
        home.join("mcp/test.json"),
        serde_json::to_vec(&json!({
            "mcpServers": {"test": {"command": "unused", "args": []}}
        }))?,
    )
    .await?;
    let loader = FAEWorkflowMetadataLoader::with_home_dir(&home);
    let mut workflow = WorkflowMetadataBuilder::new("trace", "Trace async workflow");
    workflow.start("start", "end")?;
    workflow.end("end", Some(json!("done")))?;
    loader.add(workflow.build()?)?;
    let mut builder = EngineBuilder::new();
    builder.add_runtime(PlanRuntime::new());
    builder.add_runtime(WorkflowRuntime::with_metadata_loader(loader.clone()));
    builder.add_plan_builder(WorkflowPlanBuilder::new(loader));
    builder.add_runtime(UserMemoryRuntime::with_host_dir(&home));
    builder.add_runtime(SessionRuntime::with_host_dir(&home));
    builder.add_runtime(CompressionRuntime::new("mock"));
    builder.add_runtime(MockModel::new(Arc::new(Mutex::new(Vec::new()))));
    builder.add_runtime(McpRuntime::with_mcp_dir(home.join("mcp")));
    let engine = builder.build().await;
    let (_, session) = SingleAgentEnv::from_agent_id_with_user_id("worker", "test", "alice");
    let mut scope = ContextScope::root("alice");
    scope.agent_id = Some("worker".into());
    scope.session = Some(ContextSession {
        session_id: "trace-session".into(),
        session,
    });
    let ctx = engine.ctx_with_scope(scope)?;
    assert_async_trace(
        &engine,
        &ctx,
        TaskType::Memory,
        UserMemoryRequest::Query {
            user_id: "alice".into(),
        },
        ContextStackPhase::Succeeded,
    )
    .await?;
    assert_async_trace(
        &engine,
        &ctx,
        TaskType::Session,
        SessionRequest::Query {
            agent_id: "worker".into(),
            user_id: "alice".into(),
            session_id: "trace-session".into(),
            limit: None,
            offset: None,
        },
        ContextStackPhase::Succeeded,
    )
    .await?;
    let (env, _) = WorkflowEnv::new_with_user_id("trace", Value::Null, "alice");
    assert_async_trace(
        &engine,
        &ctx,
        TaskType::Workflow,
        env,
        ContextStackPhase::Succeeded,
    )
    .await?;
    assert_async_trace(
        &engine,
        &ctx,
        TaskType::Any(COMPRESSION_TASK_TYPE.into()),
        WorkflowActionRequest {
            action: "compression".into(),
            payload: json!("compress this"),
        },
        ContextStackPhase::Succeeded,
    )
    .await?;
    // Invalid arguments fail before any external process or network operation.
    assert_async_trace(
        &engine,
        &ctx,
        TaskType::Mcp,
        McpRequest::new("test", "tool", "[]"),
        ContextStackPhase::Failed,
    )
    .await?;
    engine.exit().await?;
    tokio::fs::remove_dir_all(home).await?;
    Ok(())
}
