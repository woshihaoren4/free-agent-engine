use fae_agent::{
    FAEWorkflowMetadataLoader, WorkflowAction, WorkflowEnv, WorkflowMetadata,
    WorkflowMetadataBuilder,
};
use serde_json::{Value, json};

/// Build a child workflow whose input is resolved from the parent workflow.
fn build_child_workflow() -> anyhow::Result<WorkflowMetadata> {
    let mut child = WorkflowMetadataBuilder::new("order-validation");
    child.start("start", "end")?;
    child.end(
        "end",
        Some(json!({
            "order_id": "{$input.order_id}",
            "customer": "{$input.customer}",
            "item_count": "{$input.item_count}",
            "status": "validated"
        })),
    )?;
    child.build()
}

/// Build a parent workflow that invokes the child as one of its actions.
fn build_parent_workflow() -> anyhow::Result<WorkflowMetadata> {
    let mut parent = WorkflowMetadataBuilder::new("order-processing");
    parent.start("start", "run_child")?;
    parent.execute(
        "run_child",
        WorkflowAction::Workflow {
            workflow_id: "order-validation".to_string(),
            input: json!({
                "order_id": "{$input.order.id}",
                "customer": "{$input.order.customer}",
                "item_count": "{$input.order.item_count}"
            }),
        },
        "end",
    )?;
    parent.end(
        "end",
        Some(json!({
            "workflow": "parent",
            "child_result": "{$run_child}"
        })),
    )?;
    parent.build()
}

async fn run() -> anyhow::Result<Value> {
    let loader = FAEWorkflowMetadataLoader::new();
    let mut builder = fae_engine::EngineBuilder::new();
    builder.add_runtime(fae_engine::PlanRuntime::new());
    builder.add_runtime(fae_engine::WorkflowRuntime::with_metadata_loader(
        loader.clone(),
    ));
    builder.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(loader.clone()));
    let engine = builder.build().await;
    loader.add(build_parent_workflow()?)?;
    loader.add(build_child_workflow()?)?;
    let (env, _) = WorkflowEnv::new(
        "order-processing",
        json!({
            "order": {
                "id": "order-001",
                "customer": "Alice",
                "item_count": 3
            }
        }),
    );
    let (_, output) = engine.invoke::<_, Value>(env).await?;
    engine.exit().await?;
    Ok(output)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let output = run().await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parent_receives_child_output() -> anyhow::Result<()> {
        let output = run().await?;

        assert_eq!(
            output,
            json!({
                "workflow": "parent",
                "child_result": {
                    "order_id": "order-001",
                    "customer": "Alice",
                    "item_count": 3,
                    "status": "validated"
                }
            })
        );
        Ok(())
    }
}
