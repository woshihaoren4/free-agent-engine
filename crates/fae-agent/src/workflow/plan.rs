use std::collections::HashMap;

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    Ctx, Plan, PlanBuilderWithEnv, PlanNext, Session, SessionResponse, SingleAgentEnv,
    SingleAgentEventData, SingleAgentSession, TaskMeta, TaskReq, TaskRequest, TaskResp,
    TaskResponse, TaskType, ToolRequest, ToolRespItem, ToolResponse, WorkflowAction,
    WorkflowActionRequest, WorkflowActionResponse, WorkflowDefinition, WorkflowNode, WorkflowRun,
    WorkflowValues, to_plan_ty,
};

#[derive(Debug, Default)]
pub struct WorkflowPlanBuilder;

#[async_trait::async_trait]
impl PlanBuilderWithEnv<WorkflowRun> for WorkflowPlanBuilder {
    async fn build(
        &self,
        _rt: crate::RT,
        ctx: Ctx,
        run: WorkflowRun,
    ) -> anyhow::Result<Box<dyn Plan>> {
        run.workflow.validate()?;
        let current = run
            .workflow
            .nodes
            .iter()
            .find_map(|(id, node)| matches!(node, WorkflowNode::Start { .. }).then_some(id.clone()))
            .ok_or_else(|| anyhow::anyhow!("workflow has no start node"))?;

        Ok(Box::new(WorkflowPlan {
            id: format!("workflow-{}-{}", run.workflow.id, wd_tools::uuid::v4()),
            definition: run.workflow,
            input: run.input,
            ctx,
            current,
            outputs: HashMap::new(),
            loops: HashMap::new(),
            last_output: None,
            pending: None,
            task_sequence: 0,
            finished: false,
        }))
    }
}

#[derive(Debug)]
enum PendingAction {
    Tool,
    Session,
    SingleAgent(SingleAgentSession),
    Extension,
}

#[derive(Debug)]
struct WorkflowPlan {
    id: String,
    definition: WorkflowDefinition,
    input: Value,
    ctx: Ctx,
    current: String,
    outputs: HashMap<String, Value>,
    loops: HashMap<String, usize>,
    last_output: Option<Value>,
    pending: Option<PendingAction>,
    task_sequence: usize,
    finished: bool,
}

impl WorkflowPlan {
    fn values(&self) -> WorkflowValues<'_> {
        WorkflowValues {
            input: &self.input,
            outputs: &self.outputs,
            loops: &self.loops,
            last_output: self.last_output.as_ref(),
        }
    }

    async fn advance(&mut self) -> anyhow::Result<PlanNext> {
        loop {
            let node = self
                .definition
                .nodes
                .get(&self.current)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!("workflow node `{}` does not exist", self.current)
                })?;

            match node {
                WorkflowNode::Start { next } => self.current = next,
                WorkflowNode::End { output } => {
                    let output = match output {
                        Some(template) => self.values().resolve(&template)?,
                        None => self
                            .last_output
                            .clone()
                            .unwrap_or_else(|| self.input.clone()),
                    };
                    self.ctx.over(Box::new(output));
                    self.finished = true;
                    return Ok(PlanNext::End);
                }
                WorkflowNode::Decision {
                    condition,
                    on_true,
                    on_false,
                } => {
                    self.current = if self.values().evaluate(&condition)? {
                        on_true
                    } else {
                        on_false
                    };
                }
                WorkflowNode::Loop {
                    condition,
                    body,
                    next,
                    max_iterations,
                } => {
                    if self.values().evaluate(&condition)? {
                        let iteration = self.loops.entry(self.current.clone()).or_default();
                        anyhow::ensure!(
                            *iteration < max_iterations,
                            "loop node `{}` exceeded max_iterations ({max_iterations})",
                            self.current
                        );
                        *iteration += 1;
                        self.current = body;
                    } else {
                        self.current = next;
                    }
                }
                WorkflowNode::Execute { action, .. } => {
                    let task = self.action_task(action).await?;
                    return Ok(PlanNext::Tasks(vec![task]));
                }
            }
        }
    }

    async fn action_task(&mut self, action: WorkflowAction) -> anyhow::Result<TaskRequest> {
        anyhow::ensure!(
            self.pending.is_none(),
            "workflow already has a pending action"
        );
        self.task_sequence += 1;
        let meta = TaskMeta {
            id: format!(
                "{}-{}-{}",
                self.definition.id, self.current, self.task_sequence
            ),
            ..Default::default()
        };

        let task = match action {
            WorkflowAction::Tool {
                tool_name,
                arguments,
            } => {
                let arguments = self.values().resolve(&arguments)?;
                self.pending = Some(PendingAction::Tool);
                TaskReq {
                    ctx: self.ctx.clone(),
                    meta: TaskMeta {
                        ty: TaskType::Tool,
                        ..meta
                    },
                    req: ToolRequest::new(tool_name, serde_json::to_string(&arguments)?),
                }
                .into_request()
            }
            WorkflowAction::Session { request } => {
                let request = resolve_serializable(&self.values(), &request)?;
                self.pending = Some(PendingAction::Session);
                TaskReq {
                    ctx: self.ctx.clone(),
                    meta: TaskMeta {
                        ty: TaskType::Session,
                        ..meta
                    },
                    req: request,
                }
                .into_request()
            }
            WorkflowAction::SingleAgent {
                agent,
                prompt,
                model,
                input,
                tools,
            } => {
                let prompt = resolved_string(&self.values().resolve(&Value::String(prompt))?)?;
                let input = resolved_string(&self.values().resolve(&input)?)?;
                let (env, session) = SingleAgentEnv::new(agent, prompt, model, input, tools);
                let child = self
                    .ctx
                    .get_engine()
                    .call(
                        self.ctx.clone(),
                        to_plan_ty::<SingleAgentEnv>(),
                        Box::new(env),
                    )
                    .await?;
                self.pending = Some(PendingAction::SingleAgent(session));
                TaskReq {
                    ctx: self.ctx.clone(),
                    meta: TaskMeta {
                        ty: TaskType::Plan,
                        ..meta
                    },
                    req: child,
                }
                .into_request()
            }
            WorkflowAction::Python {
                code,
                arguments,
                task_type,
            } => {
                anyhow::ensure!(
                    !task_type.trim().is_empty(),
                    "python action task_type cannot be empty"
                );
                let request = WorkflowActionRequest {
                    action: "python".to_string(),
                    payload: json!({
                        "code": code,
                        "arguments": self.values().resolve(&arguments)?,
                    }),
                };
                self.pending = Some(PendingAction::Extension);
                extension_task(self.ctx.clone(), meta, task_type, request)
            }
            WorkflowAction::Custom { task_type, request } => {
                anyhow::ensure!(
                    !task_type.trim().is_empty(),
                    "custom action task_type cannot be empty"
                );
                let request = WorkflowActionRequest {
                    action: "custom".to_string(),
                    payload: self.values().resolve(&request)?,
                };
                self.pending = Some(PendingAction::Extension);
                extension_task(self.ctx.clone(), meta, task_type, request)
            }
        };
        Ok(task)
    }

    async fn action_output(&mut self, mut response: TaskResponse) -> anyhow::Result<Value> {
        match self
            .pending
            .take()
            .ok_or_else(|| anyhow::anyhow!("workflow received an unexpected task response"))?
        {
            PendingAction::Tool => {
                let mut response = TaskResp::<ToolResponse>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a ToolResponse"))?
                    .resp;
                loop {
                    match response.next().await? {
                        ToolRespItem::Streaming(_) => {}
                        ToolRespItem::Completed(output) => {
                            break Ok(serde_json::from_str(&output)
                                .unwrap_or_else(|_| Value::String(output)));
                        }
                    }
                }
            }
            PendingAction::Session => {
                let response = TaskResp::<SessionResponse>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a SessionResponse"))?;
                Ok(serde_json::to_value(response.resp)?)
            }
            PendingAction::SingleAgent(session) => {
                TaskResp::<()>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a child plan response"))?;
                loop {
                    let event = session.answer().await?.ok_or_else(|| {
                        anyhow::anyhow!("single-agent session ended without a final event")
                    })?;
                    match event.data {
                        SingleAgentEventData::Completed { content } => {
                            break Ok(Value::String(content));
                        }
                        SingleAgentEventData::Failed { error } => anyhow::bail!(error),
                        _ => {}
                    }
                }
            }
            PendingAction::Extension => {
                let response = TaskResp::<WorkflowActionResponse>::try_from_response(&mut response)
                    .ok_or_else(|| anyhow::anyhow!("workflow expected a WorkflowActionResponse"))?;
                Ok(response.resp.output)
            }
        }
    }
}

#[async_trait::async_trait]
impl Plan for WorkflowPlan {
    fn id(&self) -> &str {
        &self.id
    }

    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        self.advance().await
    }

    async fn next(&mut self, response: TaskResponse) -> anyhow::Result<PlanNext> {
        anyhow::ensure!(!self.finished, "workflow has already finished");
        let node_id = self.current.clone();
        let next = match self.definition.nodes.get(&node_id) {
            Some(WorkflowNode::Execute { next, .. }) => next.clone(),
            _ => anyhow::bail!("workflow is not waiting at an execute node"),
        };
        let output = self.action_output(response).await?;
        self.outputs.insert(node_id, output.clone());
        self.last_output = Some(output);
        self.current = next;
        self.advance().await
    }

    async fn abort(&mut self, _code: i32, _error: String) {
        self.pending = None;
    }
}

fn extension_task(
    ctx: Ctx,
    meta: TaskMeta,
    task_type: String,
    request: WorkflowActionRequest,
) -> TaskRequest {
    TaskReq {
        ctx,
        meta: TaskMeta {
            ty: TaskType::Any(task_type),
            ..meta
        },
        req: request,
    }
    .into_request()
}

fn resolve_serializable<T>(values: &WorkflowValues<'_>, input: &T) -> anyhow::Result<T>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    Ok(serde_json::from_value(
        values.resolve(&serde_json::to_value(input)?)?,
    )?)
}

fn resolved_string(value: &Value) -> anyhow::Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Null => Ok(String::new()),
        Value::Bool(_) | Value::Number(_) => Ok(value.to_string()),
        Value::Array(_) | Value::Object(_) => Ok(serde_json::to_string(value)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContextNull, WorkflowBuilder};
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn executes_an_extension_and_resolves_earlier_output() {
        let mut builder = WorkflowBuilder::new("example");
        builder.start("start", "A").unwrap();
        builder
            .execute(
                "A",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: json!({"name": "{$input.name}"}),
                },
                "B",
            )
            .unwrap();
        builder
            .execute(
                "B",
                WorkflowAction::Custom {
                    task_type: "fixture".to_string(),
                    request: json!({"count": "{$A.output_field.count}"}),
                },
                "end",
            )
            .unwrap();
        builder.end("end", Some(json!("{$B.done}"))).unwrap();
        let workflow = builder.build().unwrap();
        let ctx = Ctx::new(Arc::new(ContextNull));
        let mut plan = WorkflowPlanBuilder
            .build(
                crate::RT::null(),
                ctx.clone(),
                WorkflowRun::new(workflow, json!({"name": "Ada"})),
            )
            .await
            .unwrap();

        let PlanNext::Tasks(mut tasks) = plan.init().await.unwrap() else {
            panic!("expected first action");
        };
        let first = TaskReq::<WorkflowActionRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert_eq!(first.req.payload, json!({"name": "Ada"}));

        let PlanNext::Tasks(mut tasks) = plan
            .next(
                TaskResp {
                    ctx: ctx.clone(),
                    meta: first.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"output_field": {"count": 3}}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap()
        else {
            panic!("expected second action");
        };
        let second = TaskReq::<WorkflowActionRequest>::try_from_request(&mut tasks[0]).unwrap();
        assert_eq!(second.req.payload, json!({"count": 3}));

        assert!(matches!(
            plan.next(
                TaskResp {
                    ctx,
                    meta: second.meta,
                    resp: WorkflowActionResponse {
                        output: json!({"done": true}),
                    },
                }
                .into_response(),
            )
            .await
            .unwrap(),
            PlanNext::End
        ));
    }
}
