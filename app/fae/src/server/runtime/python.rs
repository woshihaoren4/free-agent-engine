use fae_agent::{
    Event, EventType, RuntimeSelectExec, TaskError, TaskReq, TaskResp, TaskType,
    WorkflowActionRequest, WorkflowActionResponse,
};
use serde_json::Value;
use wd_tools::channel::{Channel, Receiver, Sender};
const ID: &str = "workflow.python";

#[derive(Debug)]
pub struct PythonActionRuntime {
    sender: Sender<Event>,
    receiver: Receiver<Event>,
}
impl Default for PythonActionRuntime {
    fn default() -> Self {
        let (sender, receiver) = Channel::new(128);
        Self { sender, receiver }
    }
}
impl PythonActionRuntime {
    async fn execute(
        task: TaskReq<WorkflowActionRequest>,
    ) -> fae_agent::Result<TaskResp<WorkflowActionResponse>> {
        if task.req.action != "python" {
            return Err(anyhow::anyhow!("unsupported workflow action").into());
        }
        let code = task.req.payload["code"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("python action is missing code"))?;
        let arguments = task
            .req
            .payload
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Null);
        let output = tokio::select! {
            output = execute_python(code, &arguments, &task.ctx) => output?,
            reason = task.ctx.cancellation() => return Err(anyhow::anyhow!("{reason}").into()),
        };
        Ok(TaskResp {
            ctx: task.ctx,
            meta: task.meta,
            resp: WorkflowActionResponse { output },
        })
    }
}
#[async_trait::async_trait]
impl RuntimeSelectExec<WorkflowActionRequest, WorkflowActionResponse, (), ()>
    for PythonActionRuntime
{
    fn id(&self) -> &str {
        ID
    }
    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Any(ID.into())]
    }
    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.receiver.clone())
    }
    async fn spawn(&self, task: TaskReq<WorkflowActionRequest>) -> fae_agent::Result<()> {
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let ctx = task.ctx.clone();
            let meta = task.meta.clone();
            let event_type = match Self::execute(task).await {
                Ok(response) => EventType::TaskResult(response.into_response()),
                Err(error) => EventType::TaskError(TaskError {
                    ctx,
                    meta,
                    error: error.to_string(),
                }),
            };
            let _ = sender
                .send(Event {
                    from_rt_id: ID.into(),
                    event_type,
                })
                .await;
        });
        Ok(())
    }
    async fn exec(
        &self,
        task: TaskReq<WorkflowActionRequest>,
    ) -> fae_agent::Result<TaskResp<WorkflowActionResponse>> {
        Self::execute(task).await
    }
}
async fn execute_python(
    code: &str,
    arguments: &Value,
    ctx: &fae_agent::Ctx,
) -> anyhow::Result<Value> {
    let program = format!(
        "import json\narguments = json.loads({})\nresult = None\n{code}\nprint(json.dumps(result, ensure_ascii=False))\n",
        serde_json::to_string(&serde_json::to_string(arguments)?)?
    );
    let mut command = tokio::process::Command::new("python3");
    command.arg("-c").arg(program).kill_on_drop(true);
    if let Ok(scope) = ctx.scope() {
        if let Some(workspace) = &scope.workspace {
            command.current_dir(workspace);
        }
    }
    let output = command.output().await?;
    anyhow::ensure!(
        output.status.success(),
        "python action failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}
