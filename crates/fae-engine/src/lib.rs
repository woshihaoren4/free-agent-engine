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

        builder.add_runtime(PlanRuntime::new());
        builder.add_runtime(ModelRuntime::new());
        builder.add_runtime(SessionRuntime::new());

        let mut tools_runtime = ToolsRuntime::new();
        tools_runtime.add_tool(Box::new(DefaultTools::default()));
        builder.add_runtime(tools_runtime);

        builder.add_plan_builder(fae_agent::SingleAgentPlanBuilder);
        builder.add_plan_builder(fae_agent::WorkflowPlanBuilder);

        builder.build().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::{
        Ctx, EventType, TaskMeta, TaskReq, TaskResp, TaskType, ToolRequest, ToolRespItem,
        ToolResponse, WorkflowAction, WorkflowBuilder, WorkflowRun,
    };
    use serde_json::{Value, json};
    use std::time::Duration;

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
    async fn test_default_engine_executes_workflow_bits_ut() -> anyhow::Result<()> {
        let engine = Engine::default().await;
        let mut builder = WorkflowBuilder::new("read-file-workflow");
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
        let (_, output) = engine
            .invoke::<_, Value>(WorkflowRun::new(builder.build()?, input))
            .await?;

        assert_eq!(output, json!(true));
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
