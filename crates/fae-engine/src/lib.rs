mod context;
mod engine;
mod engine_rt;
mod tools;

pub use context::*;
pub use engine::*;
pub use engine_rt::*;
pub use tools::*;

impl Engine {
    pub async fn default() -> Self {
        let mut builder = EngineBuilder::new();
        let workflow_loader = fae_agent::FAEWorkflowMetadataLoader::new();

        builder.add_runtime(PlanRuntime::new());
        builder.add_runtime(WorkflowRuntime::with_metadata_loader(
            workflow_loader.clone(),
        ));
        builder.add_runtime(ModelRuntime::new());
        builder.add_runtime(CompressionRuntime::default());
        builder.add_runtime(SessionRuntime::new());
        builder.add_runtime(UserMemoryRuntime::new());
        builder.add_runtime(SkillRuntime::new());
        builder.add_runtime(McpRuntime::new());

        let mut tools_runtime = ToolsRuntime::new();
        tools_runtime.add_tool(Box::new(DefaultTools::default()));
        builder.add_runtime(tools_runtime);

        builder.add_plan_builder(fae_agent::SingleAgentPlanBuilder::new());
        builder.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(workflow_loader));

        builder.build().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::{
        Ctx, EventType, FAEWorkflowMetadataLoader, Session, SessionEventData, TaskMeta, TaskReq,
        TaskResp, TaskType, ToolInvocation, ToolRequest, ToolRespItem, ToolResponse,
        UserMemoryQuery, UserMemoryResponse, WorkflowAction, WorkflowEnv, WorkflowMetadataBuilder,
    };
    use serde_json::{Value, json};
    use std::time::Duration;

    async fn engine_with_workflow(loader: FAEWorkflowMetadataLoader) -> Engine {
        let mut builder = EngineBuilder::new();
        builder.add_runtime(PlanRuntime::new());
        builder.add_runtime(WorkflowRuntime::with_metadata_loader(loader.clone()));
        builder.add_runtime(ModelRuntime::new());
        builder.add_runtime(CompressionRuntime::default());
        builder.add_runtime(SessionRuntime::new());
        builder.add_runtime(UserMemoryRuntime::new());
        builder.add_runtime(SkillRuntime::new());
        builder.add_runtime(McpRuntime::new());

        let mut tools_runtime = ToolsRuntime::new();
        tools_runtime.add_tool(Box::new(DefaultTools::default()));
        builder.add_runtime(tools_runtime);

        builder.add_plan_builder(fae_agent::SingleAgentPlanBuilder::new());
        builder.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(loader));
        builder.build().await
    }

    fn tool_task(ctx: Ctx, tool_name: &str, arguments: Value) -> TaskReq<ToolRequest> {
        TaskReq {
            ctx,
            meta: TaskMeta {
                ty: TaskType::Tool,
                ..Default::default()
            },
            req: ToolRequest::new(tool_name.to_string(), arguments.to_string()),
        }
    }

    async fn completed_json(mut response: ToolResponse) -> anyhow::Result<Value> {
        match response.next().await? {
            ToolRespItem::Completed(output) => Ok(serde_json::from_str(&output)?),
            ToolRespItem::Streaming(output) => {
                anyhow::bail!("expected completed tool response, got streaming item: {output}")
            }
        }
    }

    #[tokio::test]
    async fn test_default_engine_executes_read_file_tool_bits_ut() -> anyhow::Result<()> {
        let engine = Engine::default().await;
        let lib_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
        let task = tool_task(
            engine.ctx(),
            READ_FILE,
            json!({
                "path": lib_path,
                "max_bytes": 256
            }),
        );

        let response = engine.rt().exec::<ToolRequest, ToolResponse>(task).await?;
        let output = completed_json(response.resp).await?;

        assert_eq!(output["truncated"], true);
        assert!(
            output["content"]
                .as_str()
                .unwrap_or_default()
                .contains("pub use tools::*;")
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_workflow_runtime_queries_metadata_by_name_bits_ut() -> anyhow::Result<()> {
        let mut metadata_builder = WorkflowMetadataBuilder::new("query-workflow");
        metadata_builder.start("start", "end")?;
        metadata_builder.end("end", Some(json!("done")))?;
        let expected = metadata_builder.build()?;

        let loader = FAEWorkflowMetadataLoader::new();
        loader.add(expected.clone())?;
        let engine = engine_with_workflow(loader).await;

        let actual = engine
            .rt()
            .select::<String, fae_agent::WorkflowMetadata>(
                TaskType::Workflow,
                "query-workflow".to_string(),
            )
            .await?;

        assert_eq!(actual.to_json()?, expected.to_json()?);
        Ok(())
    }

    #[tokio::test]
    async fn test_configured_engine_executes_workflow_bits_ut() -> anyhow::Result<()> {
        let mut builder = WorkflowMetadataBuilder::new("read-file-workflow");
        builder.start("start", "read")?;
        builder.execute(
            "read",
            WorkflowAction::Tool {
                tool_name: READ_FILE.to_string(),
                arguments: json!({
                    "path": "{$input.path}",
                    "max_bytes": 64
                }),
            },
            "end",
        )?;
        builder.end("end", Some(json!("{$read.truncated}")))?;

        let input = json!({
            "path": std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs")
        });
        let loader = FAEWorkflowMetadataLoader::new();
        let engine = engine_with_workflow(loader.clone()).await;
        loader.add(builder.build()?)?;
        let (env, session) = WorkflowEnv::new("read-file-workflow", input);
        let (_, output) = engine.invoke::<_, Value>(env).await?;

        assert_eq!(output, json!(true));
        for expected_node in ["start", "read", "end"] {
            let event = session
                .answer()
                .await?
                .ok_or_else(|| anyhow::anyhow!("workflow session ended unexpectedly"))?;
            assert_eq!(event.node_id.as_deref(), Some(expected_node));
            assert!(matches!(
                event.event_data().unwrap(),
                SessionEventData::NodeCompleted { .. }
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_workflow_is_callable_as_a_tool_bits_ut() -> anyhow::Result<()> {
        let mut workflow = WorkflowMetadataBuilder::new("tool-workflow");
        workflow.start("start", "end")?;
        workflow.end("end", Some(json!({"value": "{$input.value}"})))?;

        let loader = FAEWorkflowMetadataLoader::new();
        loader.add(workflow.build()?)?;
        let engine = engine_with_workflow(loader).await;
        let response = engine
            .rt()
            .exec::<ToolRequest, ToolResponse>(tool_task(
                engine.ctx(),
                WORKFLOW,
                json!({
                    "workflow_id": "tool-workflow",
                    "input": {"value": 42}
                }),
            ))
            .await?;

        assert_eq!(completed_json(response.resp).await?, json!({"value": 42}));
        Ok(())
    }

    #[tokio::test]
    async fn test_default_tools_expose_specialized_tools_bits_ut() -> anyhow::Result<()> {
        let engine = Engine::default().await;

        for tool_name in [AGENT, WORKFLOW, MEMORY_UPDATE] {
            let description = engine
                .rt()
                .select::<String, Value>(TaskType::Tool, tool_name.to_string())
                .await?;
            assert_eq!(description["name"], tool_name);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_memory_update_tool_writes_current_user_memory_bits_ut() -> anyhow::Result<()> {
        let host = std::env::temp_dir().join(format!(
            "fae-memory-tool-{}-{}",
            std::process::id(),
            wd_tools::uuid::v4()
        ));
        let mut builder = EngineBuilder::new();
        builder.add_runtime(UserMemoryRuntime::with_host_dir(&host));
        let mut tools = ToolsRuntime::new();
        tools.add_tool(Box::new(DefaultTools::default()));
        builder.add_runtime(tools);
        let engine = builder.build().await;

        let response = engine
            .rt()
            .exec::<ToolRequest, ToolResponse>(TaskReq {
                ctx: engine.ctx(),
                meta: TaskMeta {
                    ty: TaskType::Tool,
                    ..Default::default()
                },
                req: ToolRequest::new(
                    MEMORY_UPDATE.to_string(),
                    json!({
                        "category": "preference",
                        "content": "Prefers concise answers",
                        "confidence": "user_stated"
                    })
                    .to_string(),
                )
                .with_invocation(ToolInvocation::UserMemory {
                    user_id: "alice".to_string(),
                }),
            })
            .await?;
        let output = completed_json(response.resp).await?;
        assert_eq!(output["Updated"]["memory"]["id"], 1);

        let memories = engine
            .rt()
            .select::<_, UserMemoryResponse>(TaskType::Memory, UserMemoryQuery::new("alice"))
            .await?;
        let UserMemoryResponse::Memories { memories, .. } = memories else {
            anyhow::bail!("expected memories response");
        };
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].content, "Prefers concise answers");

        engine.exit().await?;
        let _ = tokio::fs::remove_dir_all(host).await;
        Ok(())
    }

    #[tokio::test]
    async fn test_configured_engine_executes_nested_workflow_bits_ut() -> anyhow::Result<()> {
        let mut child = WorkflowMetadataBuilder::new("child");
        child.start("start", "end")?;
        child.end("end", Some(json!("{$input.value}")))?;

        let mut parent = WorkflowMetadataBuilder::new("parent");
        parent.start("start", "nested")?;
        parent.execute(
            "nested",
            WorkflowAction::Workflow {
                workflow_id: "child".to_string(),
                input: json!({
                    "value": "{$input.child_value}"
                }),
            },
            "end",
        )?;
        parent.end("end", Some(json!({"nested": "{$nested}"})))?;
        let loader = FAEWorkflowMetadataLoader::new();
        let engine = engine_with_workflow(loader.clone()).await;
        loader.add(parent.build()?)?;
        loader.add(child.build()?)?;
        let (env, _) = WorkflowEnv::new("parent", json!({"child_value": 42}));
        let (ctx, output) = engine.invoke::<_, Value>(env).await?;

        assert_eq!(output, json!({"nested": 42}));
        assert_eq!(
            ctx.stacks().get(PlanRuntime::ID).map(Vec::len),
            Some(2),
            "parent and child plans should share the root context"
        );
        assert_eq!(ctx.stacks().get(WorkflowRuntime::ID).map(Vec::len), Some(1));
        Ok(())
    }

    #[tokio::test]
    async fn test_default_engine_spawns_list_directory_tool_bits_ut() -> anyhow::Result<()> {
        let engine = Engine::default().await;
        let rt = engine.rt();
        let receiver = rt.watch().await?;
        let task = tool_task(
            engine.ctx(),
            LIST_DIRECTORY,
            json!({
                "path": env!("CARGO_MANIFEST_DIR")
            }),
        );

        rt.spawn(task).await?;

        let event = tokio::time::timeout(Duration::from_secs(2), receiver.recv()).await??;
        let EventType::TaskResult(mut response) = event.event_type else {
            anyhow::bail!("expected task result event");
        };
        assert_eq!(response.meta.publisher, ToolsRuntime::ID);

        let response = TaskResp::<ToolResponse>::try_from_response(&mut response)
            .ok_or_else(|| anyhow::anyhow!("expected tool response"))?;
        let output = completed_json(response.resp).await?;
        let entries = output["entries"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("expected directory entries"))?;

        assert!(
            entries
                .iter()
                .any(|entry| entry["name"].as_str() == Some("Cargo.toml"))
        );

        Ok(())
    }
}
