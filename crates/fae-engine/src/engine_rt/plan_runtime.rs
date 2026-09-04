use fae_agent::{
    Ctx, Event, EventType, Plan, PlanNext, Runtime, RuntimeSelectExec, TaskError, TaskMeta,
    TaskReq, TaskResp, TaskResponse, TaskType,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, oneshot};
use wd_tools::channel::{Channel, Receiver, Sender};

const PLAN_ABORT_CODE: i32 = -1;
static NEXT_PLAN_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
enum PlanCompletion {
    Spawn(TaskMeta),
    Exec(oneshot::Sender<anyhow::Result<()>>),
}

#[derive(Debug)]
struct PlanExecution {
    plan: Box<dyn Plan>,
    ctx: Ctx,
    completion: Option<PlanCompletion>,
    pending_order: Vec<String>,
    responses: HashMap<String, TaskResponse>,
}

#[derive(Debug)]
pub struct PlanRuntime {
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
    plans: Arc<Mutex<HashMap<String, Arc<Mutex<PlanExecution>>>>>,
}

impl Default for PlanRuntime {
    fn default() -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            event_sender,
            event_receiver,
            plans: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl PlanRuntime {
    pub const ID: &'static str = "plan_default";

    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run_plan(plan: Box<dyn Plan>, ctx: Ctx) -> anyhow::Result<()> {
        let task_id = Self::next_plan_id(plan.id());
        let task = TaskReq {
            ctx: ctx.clone(),
            meta: TaskMeta {
                id: task_id,
                ty: TaskType::Plan,
                ..Default::default()
            },
            req: plan,
        };
        let mut task = task.into_request();
        Runtime::exec(&*ctx.get_engine().rt(), &mut task).await?;
        Ok(())
    }

    fn next_plan_id(plan_id: &str) -> String {
        format!("{plan_id}-{}", NEXT_PLAN_ID.fetch_add(1, Ordering::Relaxed))
    }

    async fn start_plan(
        &self,
        task: TaskReq<Box<dyn Plan>>,
        completion: PlanCompletion,
    ) -> fae_agent::Result<String> {
        let TaskReq {
            ctx,
            meta: _,
            req: mut plan,
        } = task;
        if ctx.is_aborted() {
            plan.abort(
                PLAN_ABORT_CODE,
                fae_agent::Error::ContextAborted.to_string(),
            )
            .await;
            return Err(fae_agent::Error::ContextAborted);
        }

        let execution_id = Self::next_plan_id(plan.id());
        let execution = Arc::new(Mutex::new(PlanExecution {
            plan,
            ctx,
            completion: Some(completion),
            pending_order: Vec::new(),
            responses: HashMap::new(),
        }));

        {
            let mut plans = self.plans.lock().await;
            if plans.contains_key(&execution_id) {
                return Err(anyhow::anyhow!("plan id `{execution_id}` is already running").into());
            }
            plans.insert(execution_id.clone(), execution.clone());
        }

        let next = {
            let mut execution = execution.lock().await;
            let next = execution.plan.init().await;
            if execution.ctx.is_aborted() {
                None
            } else {
                Some(next)
            }
        };
        match next {
            Some(Ok(next)) => self.advance(&execution_id, next).await,
            Some(Err(error)) => self.fail_plan(&execution_id, error).await,
            None => self.abort_plan(&execution_id).await,
        }

        Ok(execution_id)
    }

    async fn advance(&self, plan_id: &str, next: PlanNext) {
        if self.abort_if_requested(plan_id).await {
            return;
        }

        match next {
            PlanNext::End => self.finish_plan(plan_id).await,
            PlanNext::Tasks(tasks) if tasks.is_empty() => {
                self.fail_plan(
                    plan_id,
                    anyhow::anyhow!("plan `{plan_id}` produced an empty task batch"),
                )
                .await;
            }
            PlanNext::Tasks(mut tasks) => {
                let mut ids = HashSet::with_capacity(tasks.len());
                let invalid_id = tasks.iter().find_map(|task| {
                    let id = task.meta.id.clone();
                    (id.is_empty() || !ids.insert(id.clone())).then_some(id)
                });
                if let Some(invalid_id) = invalid_id {
                    let message = if invalid_id.is_empty() {
                        format!("plan `{plan_id}` produced a task without an id")
                    } else {
                        format!("plan `{plan_id}` produced duplicate task id `{invalid_id}`")
                    };
                    self.fail_plan(plan_id, anyhow::anyhow!(message)).await;
                    return;
                }

                let Some(execution) = self.plans.lock().await.get(plan_id).cloned() else {
                    return;
                };
                let ctx = {
                    let mut execution = execution.lock().await;
                    execution.pending_order =
                        tasks.iter().map(|task| task.meta.id.clone()).collect();
                    execution.responses.clear();
                    execution.ctx.clone()
                };

                for task in &mut tasks {
                    task.ctx = ctx.clone();
                    task.meta.plan_id = plan_id.to_string();
                    task.meta.publisher = Self::ID.to_string();
                }

                let rt = ctx.get_engine().rt();
                for mut task in tasks {
                    if ctx.is_aborted() {
                        self.abort_plan(plan_id).await;
                        return;
                    }
                    if let Err(error) = Runtime::spawn(&*rt, &mut task).await {
                        self.fail_plan(plan_id, error.into()).await;
                        return;
                    }
                }
            }
        }
    }

    async fn finish_plan(&self, plan_id: &str) {
        if self.abort_if_requested(plan_id).await {
            return;
        }

        let Some(execution) = self.plans.lock().await.remove(plan_id) else {
            return;
        };
        let mut execution = execution.lock().await;
        let Some(completion) = execution.completion.take() else {
            return;
        };

        match completion {
            PlanCompletion::Spawn(mut meta) => {
                if meta.publisher.is_empty() {
                    meta.publisher = Self::ID.to_string();
                }
                let event = Event {
                    from_rt_id: Self::ID.to_string(),
                    event_type: EventType::TaskResult(
                        TaskResp {
                            ctx: execution.ctx.clone(),
                            meta,
                            resp: (),
                        }
                        .into_response(),
                    ),
                };
                if let Err(error) = self.event_sender.send(event).await {
                    execution.ctx.error(error.to_string());
                }
            }
            PlanCompletion::Exec(sender) => {
                let _ = sender.send(Ok(()));
            }
        }
    }

    async fn fail_plan(&self, plan_id: &str, error: anyhow::Error) {
        let Some(execution) = self.plans.lock().await.remove(plan_id) else {
            return;
        };
        let mut execution = execution.lock().await;
        let message = error.to_string();
        execution.plan.abort(PLAN_ABORT_CODE, message.clone()).await;

        match execution.completion.take() {
            Some(PlanCompletion::Spawn(meta)) if !meta.plan_id.is_empty() => {
                let event = Event {
                    from_rt_id: Self::ID.to_string(),
                    event_type: EventType::TaskError(TaskError {
                        ctx: execution.ctx.clone(),
                        meta,
                        error: message,
                    }),
                };
                if let Err(error) = self.event_sender.send(event).await {
                    execution.ctx.error(error.to_string());
                }
            }
            Some(PlanCompletion::Spawn(_)) => execution.ctx.error(message),
            Some(PlanCompletion::Exec(sender)) => {
                let _ = sender.send(Err(anyhow::anyhow!(message)));
            }
            None => {}
        }
    }

    async fn abort_if_requested(&self, plan_id: &str) -> bool {
        let Some(execution) = self.plans.lock().await.get(plan_id).cloned() else {
            return false;
        };
        if !execution.lock().await.ctx.is_aborted() {
            return false;
        }

        self.abort_plan(plan_id).await;
        true
    }

    async fn abort_plan(&self, plan_id: &str) {
        self.fail_plan(
            plan_id,
            anyhow::Error::new(fae_agent::Error::ContextAborted),
        )
        .await;
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
        let meta = TaskMeta {
            id: task.meta.id.clone(),
            plan_id: task.meta.plan_id.clone(),
            ty: task.meta.ty.clone(),
            publisher: task.meta.publisher.clone(),
            executor: task.meta.executor.clone(),
        };
        self.start_plan(task, PlanCompletion::Spawn(meta)).await?;
        Ok(())
    }

    async fn task_result_callback(&self, response: TaskResponse) -> fae_agent::Result<()> {
        let plan_id = response.meta.plan_id.clone();
        let task_id = response.meta.id.clone();
        if response.ctx.is_aborted() {
            self.abort_plan(&plan_id).await;
            return Ok(());
        }
        let execution = self
            .plans
            .lock()
            .await
            .get(&plan_id)
            .cloned()
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;

        let responses = {
            let mut execution = execution.lock().await;
            if !execution.pending_order.contains(&task_id)
                || execution.responses.contains_key(&task_id)
            {
                None
            } else {
                execution.responses.insert(task_id.clone(), response);
                if execution.responses.len() != execution.pending_order.len() {
                    return Ok(());
                }

                let order = std::mem::take(&mut execution.pending_order);
                Some(
                    order
                        .into_iter()
                        .map(|id| {
                            execution
                                .responses
                                .remove(&id)
                                .expect("all pending responses are present")
                        })
                        .collect::<Vec<_>>(),
                )
            }
        };
        let Some(responses) = responses else {
            self.fail_plan(
                &plan_id,
                anyhow::anyhow!("unexpected callback for task `{task_id}` in plan `{plan_id}`"),
            )
            .await;
            return Ok(());
        };

        let mut generated_tasks = Vec::new();
        let mut ended = false;
        let (result, aborted) = {
            let mut execution = execution.lock().await;
            let mut result = Ok(());
            for response in responses {
                match execution.plan.next(response).await {
                    Ok(PlanNext::End) => {
                        ended = true;
                        break;
                    }
                    Ok(PlanNext::Tasks(tasks)) => generated_tasks.extend(tasks),
                    Err(error) => {
                        result = Err(error);
                        break;
                    }
                }
                if execution.ctx.is_aborted() {
                    break;
                }
            }
            (result, execution.ctx.is_aborted())
        };

        match result {
            _ if aborted => self.abort_plan(&plan_id).await,
            Err(error) => self.fail_plan(&plan_id, error).await,
            Ok(()) if ended => self.finish_plan(&plan_id).await,
            Ok(()) => {
                self.advance(&plan_id, PlanNext::Tasks(generated_tasks))
                    .await
            }
        }
        Ok(())
    }

    async fn task_error_callback(&self, error: TaskError) -> fae_agent::Result<()> {
        if error.ctx.is_aborted() {
            self.abort_plan(&error.meta.plan_id).await;
        } else {
            self.fail_plan(&error.meta.plan_id, anyhow::anyhow!(error.error))
                .await;
        }
        Ok(())
    }

    async fn exec(&self, task: TaskReq<Box<dyn Plan>>) -> fae_agent::Result<TaskResp<()>> {
        let ctx = task.ctx.clone();
        let mut meta = TaskMeta {
            id: task.meta.id.clone(),
            plan_id: task.meta.plan_id.clone(),
            ty: task.meta.ty.clone(),
            publisher: task.meta.publisher.clone(),
            executor: task.meta.executor.clone(),
        };
        let (sender, receiver) = oneshot::channel();
        self.start_plan(task, PlanCompletion::Exec(sender)).await?;
        receiver
            .await
            .map_err(|_| anyhow::anyhow!("plan completion channel closed"))??;
        meta.publisher = Self::ID.to_string();

        Ok(TaskResp {
            ctx,
            meta,
            resp: (),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EngineBuilder;
    use fae_agent::{ContextNull, TaskRequest};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

    #[derive(Debug)]
    struct AsyncStringRuntime {
        sender: Sender<Event>,
        receiver: Receiver<Event>,
    }

    impl Default for AsyncStringRuntime {
        fn default() -> Self {
            let (sender, receiver) = Channel::new(16);
            Self { sender, receiver }
        }
    }

    #[async_trait::async_trait]
    impl RuntimeSelectExec<String, String, (), ()> for AsyncStringRuntime {
        fn id(&self) -> &str {
            "async_string"
        }

        fn tys(&self) -> Vec<TaskType> {
            vec![TaskType::Any("string".to_string())]
        }

        async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
            Ok(self.receiver.clone())
        }

        async fn spawn(&self, task: TaskReq<String>) -> fae_agent::Result<()> {
            let sender = self.sender.clone();
            tokio::spawn(async move {
                let delay = if task.req == "slow" { 20 } else { 1 };
                tokio::time::sleep(Duration::from_millis(delay)).await;
                let event = Event {
                    from_rt_id: "async_string".to_string(),
                    event_type: if task.req == "error" {
                        EventType::TaskError(TaskError {
                            ctx: task.ctx,
                            meta: task.meta,
                            error: "task failed".to_string(),
                        })
                    } else {
                        EventType::TaskResult(
                            TaskResp {
                                ctx: task.ctx,
                                meta: task.meta,
                                resp: task.req,
                            }
                            .into_response(),
                        )
                    },
                };
                sender.send(event).await.unwrap();
            });
            Ok(())
        }
    }

    #[derive(Debug)]
    struct CallbackPlan {
        seen: Arc<StdMutex<Vec<String>>>,
    }

    impl CallbackPlan {
        fn task(id: &str, value: &str) -> TaskRequest {
            TaskReq {
                ctx: Ctx::new(Arc::new(ContextNull)),
                meta: TaskMeta {
                    id: id.to_string(),
                    ty: TaskType::Any("string".to_string()),
                    ..Default::default()
                },
                req: value.to_string(),
            }
            .into_request()
        }
    }

    #[async_trait::async_trait]
    impl Plan for CallbackPlan {
        fn id(&self) -> &str {
            "callback"
        }

        async fn init(&mut self) -> anyhow::Result<PlanNext> {
            Ok(PlanNext::Tasks(vec![
                Self::task("slow", "slow"),
                Self::task("fast", "fast"),
            ]))
        }

        async fn next(&mut self, mut response: TaskResponse) -> anyhow::Result<PlanNext> {
            let response = TaskResp::<String>::try_from_response(&mut response)
                .ok_or_else(|| anyhow::anyhow!("expected string response"))?;
            let mut seen = self.seen.lock().unwrap();
            seen.push(response.resp);
            if seen.len() == 2 {
                Ok(PlanNext::End)
            } else {
                Ok(PlanNext::Tasks(Vec::new()))
            }
        }

        async fn abort(&mut self, _code: i32, _error: String) {}
    }

    #[tokio::test]
    async fn exec_advances_plan_from_async_callbacks_in_batch_order() {
        let mut builder = EngineBuilder::new();
        builder.add_runtime(PlanRuntime::new());
        builder.add_runtime(AsyncStringRuntime::default());
        let engine = builder.build().await;
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let task = TaskReq {
            ctx: engine.ctx(),
            meta: TaskMeta {
                id: "unique-plan-id".to_string(),
                ty: TaskType::Plan,
                ..Default::default()
            },
            req: Box::new(CallbackPlan { seen: seen.clone() }) as Box<dyn Plan>,
        };

        tokio::time::timeout(
            Duration::from_secs(1),
            engine.rt().exec::<Box<dyn Plan>, ()>(task),
        )
        .await
        .expect("plan timed out")
        .expect("plan failed");

        assert_eq!(*seen.lock().unwrap(), vec!["slow", "fast"]);
    }

    #[derive(Debug)]
    struct ErrorPlan {
        aborted: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Plan for ErrorPlan {
        fn id(&self) -> &str {
            "error"
        }

        async fn init(&mut self) -> anyhow::Result<PlanNext> {
            Ok(PlanNext::Tasks(vec![CallbackPlan::task("error", "error")]))
        }

        async fn next(&mut self, _response: TaskResponse) -> anyhow::Result<PlanNext> {
            anyhow::bail!("error task must not produce a response")
        }

        async fn abort(&mut self, _code: i32, _error: String) {
            self.aborted.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn exec_aborts_and_returns_async_task_errors() {
        let mut builder = EngineBuilder::new();
        builder.add_runtime(PlanRuntime::new());
        builder.add_runtime(AsyncStringRuntime::default());
        let engine = builder.build().await;
        let aborted = Arc::new(AtomicBool::new(false));
        let task = TaskReq {
            ctx: engine.ctx(),
            meta: TaskMeta {
                ty: TaskType::Plan,
                ..Default::default()
            },
            req: Box::new(ErrorPlan {
                aborted: aborted.clone(),
            }) as Box<dyn Plan>,
        };

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            engine.rt().exec::<Box<dyn Plan>, ()>(task),
        )
        .await
        .expect("plan timed out")
        .expect_err("plan should fail");

        assert!(error.to_string().contains("task failed"));
        assert!(aborted.load(Ordering::SeqCst));
    }

    #[derive(Debug)]
    struct ContextAbortPlan {
        started: Arc<Notify>,
        aborted: Arc<AtomicBool>,
        next_called: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Plan for ContextAbortPlan {
        fn id(&self) -> &str {
            "context-abort"
        }

        async fn init(&mut self) -> anyhow::Result<PlanNext> {
            self.started.notify_one();
            Ok(PlanNext::Tasks(vec![CallbackPlan::task("slow", "slow")]))
        }

        async fn next(&mut self, _response: TaskResponse) -> anyhow::Result<PlanNext> {
            self.next_called.store(true, Ordering::SeqCst);
            Ok(PlanNext::End)
        }

        async fn abort(&mut self, _code: i32, _error: String) {
            self.aborted.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn context_abort_stops_plan_before_next_callback() {
        let mut builder = EngineBuilder::new();
        builder.add_runtime(PlanRuntime::new());
        builder.add_runtime(AsyncStringRuntime::default());
        let engine = builder.build().await;
        let ctx = engine.ctx();
        let started = Arc::new(Notify::new());
        let aborted = Arc::new(AtomicBool::new(false));
        let next_called = Arc::new(AtomicBool::new(false));
        let task = TaskReq {
            ctx: ctx.clone(),
            meta: TaskMeta {
                ty: TaskType::Plan,
                ..Default::default()
            },
            req: Box::new(ContextAbortPlan {
                started: started.clone(),
                aborted: aborted.clone(),
                next_called: next_called.clone(),
            }) as Box<dyn Plan>,
        };
        let rt = engine.rt();
        let execution = tokio::spawn(async move { rt.exec::<Box<dyn Plan>, ()>(task).await });

        started.notified().await;
        ctx.abort();

        let error = tokio::time::timeout(Duration::from_secs(1), execution)
            .await
            .expect("plan timed out")
            .expect("plan task panicked")
            .expect_err("aborted plan should fail");

        assert_eq!(error.to_string(), "context has been aborted");
        assert!(ctx.is_aborted());
        assert!(aborted.load(Ordering::SeqCst));
        assert!(!next_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn aborted_context_rejects_new_runtime_tasks() {
        let mut builder = EngineBuilder::new();
        builder.add_runtime(AsyncStringRuntime::default());
        let engine = builder.build().await;
        let ctx = engine.ctx();
        ctx.abort();
        let mut task = CallbackPlan::task("after-abort", "value");
        task.ctx = ctx;

        let error = Runtime::spawn(&*engine.rt(), &mut task)
            .await
            .expect_err("runtime should reject tasks after abort");

        assert!(matches!(error, fae_agent::Error::ContextAborted));
    }
}
