use fae_agent::{
    Event, EventType, RuntimeSelectExec, TaskError, TaskReq, TaskResp, TaskType,
    WorkflowActionRequest, WorkflowActionResponse,
};
use serde_json::Value;
use tokio::io::AsyncReadExt;
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
        let output = execute_python(code, &arguments, &task.ctx).await?;
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
    command
        .arg("-c")
        .arg(program)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Ok(scope) = ctx.scope() {
        if let Some(workspace) = &scope.workspace {
            command.current_dir(workspace);
        }
    }
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let status = tokio::select! {
        result = async {
            let (status, out, err) = tokio::join!(
                child.wait(), stdout.read_to_end(&mut output), stderr.read_to_end(&mut errors),
            );
            out?;
            err?;
            status
        } => result?,
        reason = ctx.cancellation() => {
            child.start_kill()?;
            child.wait().await?;
            anyhow::bail!("{reason}");
        }
    };
    anyhow::ensure!(
        status.success(),
        "python action failed: {}",
        String::from_utf8_lossy(&errors)
    );
    Ok(serde_json::from_slice(&output)?)
}
