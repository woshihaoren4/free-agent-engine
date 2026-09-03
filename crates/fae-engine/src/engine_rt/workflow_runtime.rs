use fae_agent::{
    Event, EventType, RuntimeSelectExec, TaskError, TaskReq, TaskResp, TaskType, WorkflowEnv,
    to_plan_ty,
};
use serde_json::Value;
use wd_tools::channel::{Channel, Receiver, Sender};

#[derive(Debug)]
pub struct WorkflowRuntime {
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Default for WorkflowRuntime {
    fn default() -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            event_sender,
            event_receiver,
        }
    }
}

impl WorkflowRuntime {
    pub const ID: &'static str = "workflow_default";

    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(task: TaskReq<WorkflowEnv>) -> fae_agent::Result<TaskResp<Value>> {
        let TaskReq { ctx, mut meta, req } = task;
        if meta.publisher.is_empty() {
            meta.publisher = Self::ID.to_string();
        }
        let session = req.session();
        let req = req.defer_context_completion();
        let engine = ctx.get_engine();
        let plan = engine
            .call(ctx.clone(), to_plan_ty::<WorkflowEnv>(), Box::new(req))
            .await?;

        let plan_task = TaskReq {
            ctx: ctx.clone(),
            meta: fae_agent::TaskMeta {
                ty: TaskType::Plan,
                ..Default::default()
            },
            req: plan,
        };
        engine.rt().exec::<_, ()>(plan_task).await?;
        let output = session.result().await?;

        Ok(TaskResp {
            ctx,
            meta,
            resp: output,
        })
    }
}

#[async_trait::async_trait]
impl RuntimeSelectExec<WorkflowEnv, Value, (), ()> for WorkflowRuntime {
    fn id(&self) -> &str {
        Self::ID
    }

    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Workflow]
    }

    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.event_receiver.clone())
    }

    async fn select(&self, ty: TaskType, _cond: ()) -> fae_agent::Result<()> {
        if ty != TaskType::Workflow {
            return Err(fae_agent::Error::RuntimeNoSupport);
        }
        Ok(())
    }

    async fn spawn(&self, task: TaskReq<WorkflowEnv>) -> fae_agent::Result<()> {
        let event_sender = self.event_sender.clone();
        tokio::spawn(async move {
            let mut task = task;
            if task.meta.publisher.is_empty() {
                task.meta.publisher = Self::ID.to_string();
            }
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
            let event = Event {
                from_rt_id: Self::ID.to_string(),
                event_type,
            };
            if let Err(error) = event_sender.send(event).await {
                wd_log::log_error_ln!("send workflow task result failed: {:?}", error);
            }
        });
        Ok(())
    }

    async fn exec(&self, task: TaskReq<WorkflowEnv>) -> fae_agent::Result<TaskResp<Value>> {
        Self::execute(task).await
    }
}
