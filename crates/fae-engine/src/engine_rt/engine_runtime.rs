use fae_agent::{
    Event, EventType, RT, Runtime, RuntimeSelectExec, RuntimeSelectExecWrapped, TaskRequest,
    TaskResponse, TaskType,
};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use wd_tools::channel::{Channel, Receiver, Sender};

#[derive(Debug)]
pub struct EngineRuntime {
    rts: HashMap<String, Box<dyn Runtime>>,
    rt_by_ty: HashMap<TaskType, Vec<String>>,
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

impl Default for EngineRuntime {
    fn default() -> Self {
        let (event_sender, event_receiver) = Channel::new(1024);
        Self {
            rts: HashMap::new(),
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

    pub fn add_raw_runtime(&mut self, rt: Box<dyn Runtime>) -> Option<Box<dyn Runtime>> {
        let id = rt.id().to_string();
        self.remove_runtime_tys(&id);
        self.rts.insert(id, rt)
    }

    pub fn add_raw_runtime_with_tys(
        &mut self,
        rt: Box<dyn Runtime>,
        tys: impl IntoIterator<Item = TaskType>,
    ) -> Option<Box<dyn Runtime>> {
        let id = rt.id().to_string();
        self.remove_runtime_tys(&id);
        for ty in tys {
            self.bind_task_type(ty, id.clone());
        }
        self.rts.insert(id, rt)
    }

    pub fn add_runtime<Req, Resp, Cond, Info>(
        &mut self,
        rt: Arc<dyn RuntimeSelectExec<Req, Resp, Cond, Info>>,
    ) -> Option<Box<dyn Runtime>>
    where
        Req: Debug + Send + 'static,
        Resp: Debug + Send + 'static,
        Cond: Debug + Send + 'static,
        Info: Debug + Send + 'static,
    {
        let tys = rt.tys();
        self.add_raw_runtime_with_tys(Box::new(RuntimeSelectExecWrapped::new(rt)), tys)
    }

    pub fn remove_runtime(&mut self, id: &str) -> Option<Box<dyn Runtime>> {
        self.remove_runtime_tys(id);
        self.rts.remove(id)
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

    fn runtime_by_task_type(&self, ty: &TaskType) -> Option<&dyn Runtime> {
        self.rt_by_ty
            .get(ty)
            .and_then(|ids| ids.iter().find_map(|id| self.rts.get(id)))
            .map(|rt| rt.as_ref())
    }

    fn runtime_ids_by_task_type(&self, ty: &TaskType) -> Option<&[String]> {
        self.rt_by_ty.get(ty).map(Vec::as_slice)
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
                let callback_runtime = match &event.event_type {
                    EventType::TaskResult(result)
                        if !result.meta.parent_id.is_empty()
                            && !result.meta.publisher.is_empty() =>
                    {
                        runtime.rts.get(&result.meta.publisher)
                    }
                    EventType::TaskError(error)
                        if !error.meta.parent_id.is_empty() && !error.meta.publisher.is_empty() =>
                    {
                        runtime.rts.get(&error.meta.publisher)
                    }
                    _ => None,
                };

                if let Some(callback_runtime) = callback_runtime {
                    let ctx = match &event.event_type {
                        EventType::TaskResult(result) => Some(result.ctx.clone()),
                        EventType::TaskError(error) => Some(error.ctx.clone()),
                        _ => None,
                    };
                    if let Err(error) = callback_runtime.trigger(&mut event).await {
                        if let Some(ctx) = ctx {
                            ctx.error(error.to_string());
                        }
                        wd_log::log_error_ln!("dispatch task result callback failed: {:?}", error);
                    }
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
            .runtime_by_task_type(&ty)
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;
        rt.select(ty, cond).await
    }

    async fn spawn(&self, task: &mut TaskRequest) -> fae_agent::Result<()> {
        if task.meta.executor.is_empty() {
            let Some(rt_ids) = self.runtime_ids_by_task_type(&task.meta.ty) else {
                return Err(fae_agent::Error::RuntimeNoSupport);
            };

            let original_executor = task.meta.executor.clone();
            for rt_id in rt_ids {
                let Some(rt) = self.rts.get(rt_id) else {
                    continue;
                };

                task.meta.executor = rt_id.clone();
                match rt.spawn(task).await {
                    Err(fae_agent::Error::RuntimeNoSupport) => continue,
                    result => return result,
                }
            }
            task.meta.executor = original_executor;
            return Err(fae_agent::Error::RuntimeNoSupport);
        }

        let rt = self
            .rts
            .get(&task.meta.executor)
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;
        rt.spawn(task).await
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
        if task.meta.executor.is_empty() {
            let Some(rt_ids) = self.runtime_ids_by_task_type(&task.meta.ty) else {
                return Err(fae_agent::Error::RuntimeNoSupport);
            };

            let original_executor = task.meta.executor.clone();
            for rt_id in rt_ids {
                let Some(rt) = self.rts.get(rt_id) else {
                    continue;
                };

                task.meta.executor = rt_id.clone();
                match rt.exec(task).await {
                    Err(fae_agent::Error::RuntimeNoSupport) => continue,
                    result => return result,
                }
            }
            task.meta.executor = original_executor;
            return Err(fae_agent::Error::RuntimeNoSupport);
        }

        let rt = self
            .rts
            .get(&task.meta.executor)
            .ok_or(fae_agent::Error::RuntimeNoSupport)?;
        rt.exec(task).await
    }

    async fn kill(&self, ty: TaskType, rtid: &str, task_id: &str) -> fae_agent::Result<()> {
        let rt = if rtid.is_empty() {
            self.runtime_by_task_type(&ty)
        } else {
            self.rts.get(rtid).map(|rt| rt.as_ref())
        }
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
