//! Session orchestration and public result conversion live outside core execution.
use crate::{Engine, PlanRuntime};
use fae_agent::protocol::{ContentBlock, ErrorInfo};
use fae_agent::{ContextState, ExecutionHandle, ExecutionOutcome, PlanCallMeta};

pub async fn execute_session<Env: std::fmt::Debug + Send + Sync + 'static>(
    engine: &Engine,
    execution: ExecutionHandle,
    env: Env,
) -> anyhow::Result<()> {
    let plan = engine
        .call(execution.ctx.clone(), fae_agent::to_plan_ty::<Env>(), env)
        .await;
    finish_execution(execution, plan).await
}

pub async fn execute_session_plan(
    engine: &Engine,
    execution: ExecutionHandle,
    plan_id: String,
    env: serde_json::Value,
) -> anyhow::Result<()> {
    let plan = fae_agent::Engine::build_plan(
        engine,
        execution.ctx.clone(),
        PlanCallMeta { plan_id },
        Box::new(env),
    )
    .await;
    finish_execution(execution, plan).await
}

async fn finish_execution(
    execution: ExecutionHandle,
    plan: anyhow::Result<Box<dyn fae_agent::Plan>>,
) -> anyhow::Result<()> {
    let ctx = execution.ctx.clone();
    let result = match plan {
        Ok(mut plan) => {
            if ctx.cancel_requested() {
                plan.abort(-1, "execution cancelled during preparation".into())
                    .await;
                let _ = ctx.cleanup().await;
                let _ = ctx.set_state(ContextState::cancelled(
                    "execution cancelled during preparation",
                ));
                Err(anyhow::anyhow!("execution cancelled during preparation"))
            } else {
                match execution.session.running(&execution.execution_id) {
                    Ok(()) => PlanRuntime::run_plan(plan, ctx.clone()).await,
                    Err(error) => {
                        plan.abort(-1, error.to_string()).await;
                        let _ = ctx.cleanup().await;
                        let _ = ctx.set_state(ContextState::cancelled(error.to_string()));
                        Err(error.into())
                    }
                }
            }
        }
        Err(error) => Err(error),
    };
    let outcome = match &*ctx.state().current() {
        ContextState::Succeeded { output } if result.is_ok() => match output.json_snapshot() {
            Ok(value) => ExecutionOutcome::Succeeded {
                result: vec![ContentBlock::Json { value }],
            },
            Err(error) => ExecutionOutcome::Failed {
                error: ErrorInfo {
                    code: "unsupported_result".into(),
                    message: error.to_string(),
                },
            },
        },
        ContextState::Cancelled { reason } => ExecutionOutcome::Cancelled {
            reason: reason.to_string(),
        },
        _ => ExecutionOutcome::Failed {
            error: ErrorInfo {
                code: "execution_failed".into(),
                message: result
                    .as_ref()
                    .err()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "plan did not produce a public result".into()),
            },
        },
    };
    let _ = ctx.invocation_finished();
    execution
        .session
        .finishing(&execution.execution_id, outcome)?;
    result
}
