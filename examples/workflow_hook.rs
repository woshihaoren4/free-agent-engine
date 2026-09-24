use std::sync::Arc;

use fae_agent::{
    FAEWorkflowMetadataLoader, WorkflowCondition, WorkflowEnv, WorkflowHook, WorkflowHookBuilder,
    WorkflowHookContext, WorkflowHookPhase, WorkflowMetadata, WorkflowMetadataBuilder,
};
use fae_engine::{EngineBuilder, PlanRuntime, WorkflowRuntime};
use serde_json::{Value, json};

#[derive(Debug)]
struct TraceHookBuilder;

#[async_trait::async_trait]
impl WorkflowHookBuilder for TraceHookBuilder {
    async fn build(&self) -> Arc<dyn WorkflowHook> {
        Arc::new(TraceHook)
    }
}

#[derive(Debug)]
struct TraceHook;

impl TraceHook {
    fn trace(&self, callback: &str, ctx: &WorkflowHookContext<'_>) {
        let phase = match ctx.phase {
            WorkflowHookPhase::Before => "before".to_string(),
            WorkflowHookPhase::After { output } => format!(
                "after output={}",
                output
                    .map(Value::to_string)
                    .unwrap_or_else(|| "none".to_string())
            ),
            WorkflowHookPhase::Failed { error } => format!("failed error={error}"),
        };
        println!(
            "[hook] {callback}: workflow={}, plan={}, node={}, {phase}",
            ctx.workflow_id, ctx.plan_id, ctx.node_id
        );
    }
}

#[async_trait::async_trait]
impl WorkflowHook for TraceHook {
    async fn on_start(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        self.trace("start", ctx);
        Ok(())
    }

    async fn on_parallel_start(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        self.trace("parallel_start", ctx);
        Ok(())
    }

    async fn on_execute(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        self.trace("execute", ctx);
        Ok(())
    }

    async fn on_decision(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        self.trace("decision", ctx);
        Ok(())
    }

    async fn on_loop(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        self.trace("loop", ctx);
        Ok(())
    }

    async fn on_end(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        self.trace("end", ctx);
        Ok(())
    }

    async fn on_join_end(&self, ctx: &WorkflowHookContext<'_>) -> anyhow::Result<()> {
        self.trace("join_end", ctx);
        Ok(())
    }
}

fn build_workflow() -> anyhow::Result<WorkflowMetadata> {
    let mut builder = WorkflowMetadataBuilder::new("hook-example", "Demonstrate workflow hooks");
    builder.start("start", "check")?;
    builder.decision(
        "check",
        WorkflowCondition::Truthy {
            value: json!("{$input.approved}"),
        },
        "end",
        "end",
    )?;
    builder.end(
        "end",
        Some(json!({
            "approved": "{$input.approved}",
            "message": "{$input.message}"
        })),
    )?;
    builder.build()
}

async fn build_engine(loader: FAEWorkflowMetadataLoader) -> fae_engine::Engine {
    let mut builder = EngineBuilder::new();
    builder.add_runtime(PlanRuntime::new());
    builder.add_runtime(WorkflowRuntime::with_metadata_loader(loader.clone()));

    let mut workflow_builder = fae_agent::WorkflowPlanBuilder::new(loader);
    workflow_builder.add_hook(TraceHookBuilder);
    builder.add_plan_builder(workflow_builder);

    builder.build().await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let loader = FAEWorkflowMetadataLoader::new();
    loader.add(build_workflow()?)?;
    let engine = build_engine(loader).await;

    let (env, _) = WorkflowEnv::new(
        "hook-example",
        json!({
            "approved": true,
            "message": "workflow hooks are active"
        }),
    );
    let (_, output) = engine.invoke::<_, Value>(env).await?;
    println!(
        "workflow output: {}",
        serde_json::to_string_pretty(&output)?
    );

    engine.exit().await?;
    Ok(())
}
