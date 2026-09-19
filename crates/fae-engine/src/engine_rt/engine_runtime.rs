use fae_agent::{
    Event, EventType, RT, Runtime, RuntimeSelectExec, RuntimeSelectExecWrapped, TaskRequest,
    TaskResponse, TaskType, hook::runtime::RuntimeHookBuilder,
};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use wd_tools::channel::{Channel, Receiver, Sender};

pub struct EngineRuntime {
    rts: HashMap<String, Arc<dyn Runtime>>,
    hooks: HashMap<TaskType, Vec<Box<dyn RuntimeHookBuilder>>>,
    rt_by_ty: HashMap<TaskType, Vec<String>>,
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Debug for EngineRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineRuntime")
            .field("rts", &self.rts)
            .field("hook_task_types", &self.hooks.keys().collect::<Vec<_>>())
            .field("rt_by_ty", &self.rt_by_ty)
            .field("event_sender", &self.event_sender)
            .field("event_receiver", &self.event_receiver)
            .finish()
    }
}

impl Default for EngineRuntime {
    fn default() -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            rts: HashMap::new(),
            hooks: HashMap::new(),
            rt_by_ty: HashMap::new(),
            event_sender,
            event_receiver,
        }
    }
}

impl EngineRuntime {
    pub const ID: &'static str = "engine";

    pub fn new() -> Self {
        Self::default()
    }

    pub async fn build(self) -> RT {
        let mut receivers = Vec::new();
        for rt in self.rts.values() {
            let Ok(rt_receiver) = rt.watch().await else {
                continue;
            };
            receivers.push(rt_receiver);
        }

        let runtime = Arc::new(self);
        for receiver in receivers {
            Self::forward_events(receiver, runtime.clone());
        }

        RT::new(runtime)
    }

    pub fn event_sender(&self) -> Sender<Event> {
        self.event_sender.clone()
    }

    pub fn add_raw_runtime(&mut self, rt: Box<dyn Runtime>) -> Option<Arc<dyn Runtime>> {
        let id = rt.id().to_string();
        self.remove_runtime_tys(&id);
        self.rts.insert(id, Arc::from(rt))
    }

    pub fn add_raw_runtime_with_tys(
        &mut self,
        rt: Box<dyn Runtime>,
        tys: impl IntoIterator<Item = TaskType>,
    ) -> Option<Arc<dyn Runtime>> {
        let id = rt.id().to_string();
        self.remove_runtime_tys(&id);
        for ty in tys {
            self.bind_task_type(ty, id.clone());
        }
        self.rts.insert(id, Arc::from(rt))
    }

    pub fn add_runtime<Req, Resp, Cond, Info>(
        &mut self,
        rt: Arc<dyn RuntimeSelectExec<Req, Resp, Cond, Info>>,
    ) -> Option<Arc<dyn Runtime>>
    where
        Req: Debug + Send + 'static,
        Resp: Debug + Send + 'static,
        Cond: Debug + Send + 'static,
        Info: Debug + Send + 'static,
    {
        let tys = rt.tys();
        self.add_raw_runtime_with_tys(Box::new(RuntimeSelectExecWrapped::new(rt)), tys)
    }

    pub fn remove_runtime(&mut self, id: &str) -> Option<Arc<dyn Runtime>> {
        self.remove_runtime_tys(id);
        self.rts.remove(id)
    }

    pub fn add_runtime_hook(&mut self, ty: TaskType, hook: Box<dyn RuntimeHookBuilder>) {
        self.hooks.entry(ty).or_default().push(hook);
    }

    pub fn remove_runtime_hooks(
        &mut self,
        ty: &TaskType,
    ) -> Option<Vec<Box<dyn RuntimeHookBuilder>>> {
        self.hooks.remove(ty)
    }

    pub fn bind_task_type(&mut self, ty: TaskType, rt_id: impl Into<String>) {
        let rt_id = rt_id.into();
        let ids = self.rt_by_ty.entry(ty).or_default();
        if !ids.contains(&rt_id) {
            ids.push(rt_id);
        }
    }

    pub fn unbind_task_type(&mut self, ty: &TaskType) -> Option<Vec<String>> {
        self.rt_by_ty.remove(ty)
    }

    pub fn runtime(&self, id: &str) -> Option<&dyn Runtime> {
        self.rts.get(id).map(|rt| rt.as_ref())
    }

    pub fn contains_runtime(&self, id: &str) -> bool {
        self.rts.contains_key(id)
    }

    fn runtime_ids_by_task_type(&self, ty: &TaskType) -> Option<&[String]> {
        self.rt_by_ty.get(ty).map(Vec::as_slice)
    }

    async fn runtimes_for_task(
        &self,
        ty: &TaskType,
        runtime_id: Option<&str>,
    ) -> Vec<Arc<dyn Runtime>> {
        let mut runtimes = match runtime_id {
            Some(id) => self.rts.get(id).cloned().into_iter().collect(),
            None => self
                .runtime_ids_by_task_type(ty)
                .into_iter()
                .flatten()
                .filter_map(|id| self.rts.get(id).cloned())
                .collect(),
        };

        if let Some(hooks) = self.hooks.get(ty) {
            for hook in hooks {
                runtimes = vec![hook.build(runtimes).await];
            }
        }

        runtimes
    }

    fn remove_runtime_tys(&mut self, id: &str) {
        self.rt_by_ty.retain(|_, rt_ids| {
            rt_ids.retain(|rt_id| rt_id != id);
            !rt_ids.is_empty()
        });
    }

    fn forward_events(receiver: Receiver<Event>, runtime: Arc<Self>) {
        tokio::spawn(async move {
            while let Ok(mut event) = receiver.recv().await {
                let callback_runtime_id = match &event.event_type {
                    EventType::TaskResult(result)
                        if !result.meta.plan_id.is_empty() && !result.meta.publisher.is_empty() =>
                    {
                        Some(result.meta.publisher.clone())
                    }
                    EventType::TaskError(error)
                        if !error.meta.plan_id.is_empty() && !error.meta.publisher.is_empty() =>
                    {
                        Some(error.meta.publisher.clone())
                    }
                    _ => None,
                }
                .filter(|id| runtime.rts.contains_key(id));

                if let Some(callback_runtime_id) = callback_runtime_id {
                    let ctx = match &event.event_type {
                        EventType::TaskResult(result) => Some(result.ctx.clone()),
                        EventType::TaskError(error) => Some(error.ctx.clone()),
                        _ => None,
                    };
                    let callback_owner = runtime.clone();
                    tokio::spawn(async move {
                        let callback_runtime = callback_owner
                            .rts
                            .get(&callback_runtime_id)
                            .expect("callback runtime disappeared after engine build");
                        if let Err(error) = callback_runtime.trigger(&mut event).await {
                            if let Some(ctx) = ctx {
                                ctx.error(error.to_string());
                            }
                            wd_log::log_error_ln!(
                                "dispatch task result callback failed: {:?}",
                                error
                            );
                        }
                    });
                } else {
                    if let EventType::TaskError(error) = &event.event_type {
                        error.ctx.error(error.error.clone());
                    }
                    if runtime.event_sender.send(event).await.is_err() {
                        break;
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fae_agent::{ContextNull, TaskMeta, TaskReq, TaskResp};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    #[derive(Debug)]
    struct EventSourceRuntime {
        receiver: Receiver<Event>,
    }

    #[async_trait::async_trait]
    impl Runtime for EventSourceRuntime {
        fn id(&self) -> &str {
            "event_source"
        }

        async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
            Ok(self.receiver.clone())
        }
    }

    #[derive(Debug)]
    struct BlockingCallbackRuntime {
        slow_started: Arc<Notify>,
        release_slow: Arc<Notify>,
        fast_finished: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl Runtime for BlockingCallbackRuntime {
        fn id(&self) -> &str {
            "blocking_callback"
        }

        async fn trigger(&self, event: &mut Event) -> fae_agent::Result<()> {
            let EventType::TaskResult(response) = &event.event_type else {
                return Err(fae_agent::Error::RuntimeNoSupport);
            };
            match response.meta.id.as_str() {
                "slow" => {
                    self.slow_started.notify_one();
                    self.release_slow.notified().await;
                }
                "fast" => self.fast_finished.notify_one(),
                _ => return Err(fae_agent::Error::RuntimeNoSupport),
            }
            Ok(())
        }
    }

    #[derive(Debug)]
    struct CountingRuntime {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Runtime for CountingRuntime {
        fn id(&self) -> &str {
            "counting"
        }

        async fn spawn(&self, _task: &mut TaskRequest) -> fae_agent::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct CountingHookBuilder {
        builds: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl RuntimeHookBuilder for CountingHookBuilder {
        async fn build(&self, runtimes: Vec<Arc<dyn Runtime>>) -> Arc<dyn Runtime> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            Arc::new(CountingHookRuntime {
                runtimes,
                calls: self.calls.clone(),
            })
        }
    }

    #[derive(Debug)]
    struct CountingHookRuntime {
        runtimes: Vec<Arc<dyn Runtime>>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Runtime for CountingHookRuntime {
        fn id(&self) -> &str {
            "counting_hook"
        }

        async fn spawn(&self, task: &mut TaskRequest) -> fae_agent::Result<()> {
            if task.meta.executor != self.id() {
                return Err(fae_agent::Error::RuntimeNoSupport);
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.runtimes
                .first()
                .ok_or(fae_agent::Error::RuntimeNoSupport)?
                .spawn(task)
                .await
        }
    }

    fn callback_event(id: &str) -> Event {
        Event {
            from_rt_id: "event_source".to_string(),
            event_type: EventType::TaskResult(
                TaskResp {
                    ctx: fae_agent::Ctx::new(Arc::new(ContextNull)),
                    meta: TaskMeta {
                        id: id.to_string(),
                        plan_id: format!("{id}-plan"),
                        publisher: "blocking_callback".to_string(),
                        ..Default::default()
                    },
                    resp: (),
                }
                .into_response(),
            ),
        }
    }

    #[tokio::test]
    async fn forwards_callbacks_concurrently() {
        let (sender, receiver) = Channel::new(2);
        let slow_started = Arc::new(Notify::new());
        let release_slow = Arc::new(Notify::new());
        let fast_finished = Arc::new(Notify::new());
        let mut runtime = EngineRuntime::new();
        runtime.add_raw_runtime(Box::new(EventSourceRuntime { receiver }));
        runtime.add_raw_runtime(Box::new(BlockingCallbackRuntime {
            slow_started: slow_started.clone(),
            release_slow: release_slow.clone(),
            fast_finished: fast_finished.clone(),
        }));
        let _runtime = runtime.build().await;

        sender.send(callback_event("slow")).await.unwrap();
        slow_started.notified().await;
        sender.send(callback_event("fast")).await.unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(1), fast_finished.notified())
            .await
            .expect("fast callback was blocked by slow callback");
        release_slow.notify_one();
    }

    #[tokio::test]
    async fn builds_hook_runtime_for_every_task_call() {
        let builds = Arc::new(AtomicUsize::new(0));
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let runtime_calls = Arc::new(AtomicUsize::new(0));
        let mut runtime = EngineRuntime::new();
        runtime.add_raw_runtime_with_tys(
            Box::new(CountingRuntime {
                calls: runtime_calls.clone(),
            }),
            [TaskType::Tool],
        );
        runtime.add_runtime_hook(
            TaskType::Tool,
            Box::new(CountingHookBuilder {
                builds: builds.clone(),
                calls: hook_calls.clone(),
            }),
        );

        for (id, executor) in [("first", ""), ("second", "counting")] {
            let mut task = TaskReq {
                ctx: fae_agent::Ctx::new(Arc::new(ContextNull)),
                meta: TaskMeta {
                    id: id.to_string(),
                    ty: TaskType::Tool,
                    executor: executor.to_string(),
                    ..Default::default()
                },
                req: (),
            }
            .into_request();
            runtime.spawn(&mut task).await.unwrap();
        }

        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert_eq!(hook_calls.load(Ordering::SeqCst), 2);
        assert_eq!(runtime_calls.load(Ordering::SeqCst), 2);
    }
}

#[async_trait::async_trait]
impl Runtime for EngineRuntime {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.event_receiver.clone())
    }

    async fn select(
        &self,
        ty: TaskType,
        cond: &mut Box<dyn Any + Send>,
    ) -> fae_agent::Result<Box<dyn Any + Send>> {
        let rt = self
            .runtimes_for_task(&ty, None)
            .await
            .into_iter()
            .next()
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;
        rt.select(ty, cond).await
    }

    async fn spawn(&self, task: &mut TaskRequest) -> fae_agent::Result<()> {
        if task.ctx.is_aborted() {
            return Err(fae_agent::Error::ContextAborted);
        }

        let ty = task.meta.ty.clone();
        let original_executor = task.meta.executor.clone();
        let runtime_id = (!original_executor.is_empty()).then_some(original_executor.as_str());
        let runtimes = self.runtimes_for_task(&ty, runtime_id).await;

        for rt in runtimes {
            task.meta.executor = rt.id().to_string();
            match rt.spawn(task).await {
                Err(fae_agent::Error::RuntimeNoSupport) => continue,
                result => return result,
            }
        }

        task.meta.executor = original_executor;
        Err(fae_agent::Error::RuntimeNoSupport)
    }

    async fn trigger(&self, event: &mut Event) -> fae_agent::Result<()> {
        if let EventType::Task(task) = &mut event.event_type {
            return self.spawn(task).await;
        }

        let rt_id = match &event.event_type {
            EventType::Task(_) => unreachable!(),
            EventType::TaskResult(result) => result.meta.publisher.clone(),
            EventType::TaskError(error) => error.meta.publisher.clone(),
            EventType::Any(rt_id, _) => rt_id.clone(),
        };

        let rt = self
            .rts
            .get(&rt_id)
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;
        rt.trigger(event).await
    }

    async fn exec(&self, task: &mut TaskRequest) -> fae_agent::Result<TaskResponse> {
        if task.ctx.is_aborted() {
            return Err(fae_agent::Error::ContextAborted);
        }

        let ty = task.meta.ty.clone();
        let original_executor = task.meta.executor.clone();
        let runtime_id = (!original_executor.is_empty()).then_some(original_executor.as_str());
        let runtimes = self.runtimes_for_task(&ty, runtime_id).await;

        for rt in runtimes {
            task.meta.executor = rt.id().to_string();
            match rt.exec(task).await {
                Err(fae_agent::Error::RuntimeNoSupport) => continue,
                result => return result,
            }
        }

        task.meta.executor = original_executor;
        Err(fae_agent::Error::RuntimeNoSupport)
    }

    async fn kill(&self, ty: TaskType, rtid: &str, task_id: &str) -> fae_agent::Result<()> {
        let runtime_id = (!rtid.is_empty()).then_some(rtid);
        let rt = self
            .runtimes_for_task(&ty, runtime_id)
            .await
            .into_iter()
            .next()
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;

        rt.kill(ty, rt.id(), task_id).await
    }

    async fn exit(&self) -> fae_agent::Result<()> {
        let mut first_err = None;

        for rt in self.rts.values() {
            if let Err(err) = rt.exit().await {
                first_err.get_or_insert(err);
            }
        }

        match first_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}
