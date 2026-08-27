use fae_agent::{
    Ctx, Event, EventType, Plan, PlanNext, Runtime, RuntimeSelectExec, TaskReq, TaskResp, TaskType,
};
use wd_tools::channel::{Channel, Receiver, Sender};

const PLAN_ABORT_CODE: i32 = -1;

#[derive(Debug)]
pub struct PlanRuntime {
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Default for PlanRuntime {
    fn default() -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            event_sender,
            event_receiver,
        }
    }
}

impl PlanRuntime {
    pub const ID: &'static str = "plan_default";

    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run_plan(mut plan: Box<dyn Plan>, ctx: Ctx) -> anyhow::Result<()> {
        let result = Self::run_plan_inner(plan.as_mut(), &ctx).await;
        if let Err(error) = &result {
            plan.abort(PLAN_ABORT_CODE, error.to_string()).await;
        }
        result
    }

    async fn run_plan_inner(plan: &mut dyn Plan, ctx: &Ctx) -> anyhow::Result<()> {
        let mut next = plan.init().await?;

        loop {
            let tasks = match next {
                PlanNext::End => return Ok(()),
                PlanNext::Tasks(tasks) if tasks.is_empty() => {
                    anyhow::bail!("plan `{}` produced an empty task batch", plan.id())
                }
                PlanNext::Tasks(tasks) => tasks,
            };

            let mut responses = Vec::with_capacity(tasks.len());
            for mut task in tasks {
                task.ctx = ctx.clone();
                let rt = ctx.get_rt();
                let mut response = Runtime::exec(&*rt, &mut task).await?;
                response.ctx = ctx.clone();
                responses.push(response);
            }

            let mut generated_tasks = Vec::new();
            for response in responses {
                match plan.next(response).await? {
                    PlanNext::End => return Ok(()),
                    PlanNext::Tasks(tasks) => generated_tasks.extend(tasks),
                }
            }

            next = PlanNext::Tasks(generated_tasks);
        }
    }
}

#[async_trait::async_trait]
impl RuntimeSelectExec<Box<dyn Plan>, (), (), ()> for PlanRuntime {
    fn id(&self) -> &str {
        Self::ID
    }

    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Plan]
    }

    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.event_receiver.clone())
    }

    async fn select(&self, ty: TaskType, _cond: ()) -> fae_agent::Result<()> {
        if ty != TaskType::Plan {
            return Err(fae_agent::Error::RuntimeNoSupport);
        }
        Ok(())
    }

    async fn spawn(&self, task: TaskReq<Box<dyn Plan>>) -> fae_agent::Result<()> {
        let TaskReq {
            ctx,
            mut meta,
            req: plan,
        } = task;
        let event_sender = self.event_sender.clone();

        tokio::spawn(async move {
            let runtime_id = Self::ID.to_string();
            let response_ctx = ctx.clone();
            let event_ctx = response_ctx.clone();
            let result = Self::run_plan(plan, ctx).await.map(|()| {
                meta.publisher = runtime_id.clone();
                Event {
                    from_rt_id: runtime_id,
                    event_type: EventType::TaskResult(
                        TaskResp {
                            ctx: event_ctx,
                            meta,
                            resp: (),
                        }
                        .into_response(),
                    ),
                }
            });

            match result {
                Ok(event) => {
                    if let Err(err) = event_sender.send(event).await {
                        wd_log::log_error_ln!("send plan task result failed: {:?}", err);
                    }
                }
                Err(err) => response_ctx.error(err.to_string()),
            }
        });

        Ok(())
    }

    async fn exec(&self, task: TaskReq<Box<dyn Plan>>) -> fae_agent::Result<TaskResp<()>> {
        let TaskReq {
            ctx,
            mut meta,
            req: plan,
        } = task;

        Self::run_plan(plan, ctx.clone()).await?;
        meta.publisher = Self::ID.to_string();

        Ok(TaskResp {
            ctx,
            meta,
            resp: (),
        })
    }
}
