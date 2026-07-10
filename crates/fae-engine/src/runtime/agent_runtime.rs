use crate::tools::{AGENT_TASK_TOOL_NAME, AgentTaskTool};
use crate::{IdenInfo, Tool};
use fae_agent::{AgentTask, AgentTaskStatus, AgentTasks, Env, EnvEvent, Environment, Select, TaskResult, TaskReq, Thing, ThingItem, ThingSelect, ToolRequest, ToolResponse, TkTy};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::RwLock;
use wd_tools::PFErr;
use wd_tools::channel::Channel;

const AGENT_TASK_RUNTIME_ID: &str = "AGENT_TASK_RUNTIME_ID";

#[async_trait::async_trait]
pub trait AgentTaskStore: Debug {
    async fn push_task(&self, task: AgentTask);
    async fn update_status_executing(&self, task_id: &str) -> anyhow::Result<()>;
    async fn complete_task(
        &self,
        task_id: &str,
        status: String,
        result: String,
        ext: Option<Box<dyn Any + Send + Sync + 'static>>,
    ) -> anyhow::Result<AgentTasks>;
}

#[derive(Debug)]
pub struct AgentRuntime {
    pub task_store: Arc<dyn AgentTaskStore + Send + Sync + 'static>,
    pub task_tool: AgentTaskTool,
    pub channel: Channel<AgentTasks>,
    pub task_result_channel: Channel<TaskResult>,
    parent: Option<Env>,
}

impl AgentRuntime {
    pub fn new(store: impl AgentTaskStore + Send + Sync + 'static) -> Self {
        let chan = Channel::with_cap(100);
        let task_store = Arc::new(store);
        let task_tool = AgentTaskTool::new(chan.clone(), task_store.clone());
        let task_result_channel = Channel::with_cap(100);
        Self {
            task_store,
            task_tool,
            parent: None,
            channel: chan,
            task_result_channel,
        }
    }
    pub async fn exec_tool(&self, mut task: fae_agent::Task) -> anyhow::Result<TaskResult> {
        if let Some(req) = task.into_inner::<ToolRequest>() {
            let iden = IdenInfo::new(
                task.id.clone(),
                task.agent_id.clone(),
                task.user_id.clone(),
                task.ctx,
            );
            match self.task_tool.call(iden, req.arguments).await {
                Ok(resp) => Ok(TaskResult::success(task.id, task.agent_id).set_data(resp)),
                Err(e) => {
                    let info = format!("Tool[{}] call failed. error: {}", req.tool_name, e);
                    Ok(TaskResult::success(task.id, task.agent_id)
                        .set_data(ToolResponse::with_result(info)))
                }
            }
        } else {
            anyhow::anyhow!("[TaskRuntime] task is not a tool request").err()
        }
    }
    pub fn is_task_tool_task(task: &fae_agent::Task) -> bool {
        task.get_type() == TkTy::Tool
            && task.get_exec_channel() == "default"
            && task.deref_args::<ToolRequest, bool>(|x| {
                if let Some(r) = x {
                    r.tool_name.as_str() == AGENT_TASK_TOOL_NAME
                } else {
                    false
                }
            })
    }
}

#[async_trait::async_trait]
impl Environment for AgentRuntime {
    fn id(&self) -> &'static str {
        AGENT_TASK_RUNTIME_ID
    }

    async fn register_parent_env(&mut self, env: Env) {
        self.parent = Some(env);
    }

    async fn watch(&self) -> anyhow::Result<EnvEvent> {
        loop {
            let parent_fut = if let Some(ref p) = self.parent {
                Some(p.watch())
            } else {
                None
            };
            let recv = self.channel.recv();
            let task_result_recv = self.task_result_channel.recv();

            tokio::select! {
                res = recv => {
                    let task = res?;
                    match task.first_task_status() {
                        AgentTaskStatus::CREATE => {
                            return Ok(EnvEvent::Agent(task));
                        }
                        AgentTaskStatus::COMPLETED | AgentTaskStatus::FAILED => {
                            let task = TaskResult::success(task.first_task_id(), task.first_task_author_id()).set_data(task);
                            return Ok(EnvEvent::TaskResult(task));
                        }
                        _ => {
                            // 其他状态，不处理
                        }
                    }
                }
                res = task_result_recv => {
                    let task_result = res?;
                    return Ok(EnvEvent::TaskResult(task_result));
                }
                res = async {
                    if let Some(fut) = parent_fut {
                        fut.await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    return res;
                }
            };
        }
    }

    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>> {
        if let ThingSelect::Tool(chan, name) = &select.select {
            if name == AGENT_TASK_TOOL_NAME && chan == "default" {
                return Ok(vec![
                    Thing::new(self.id().to_string())
                        .add_item(ThingItem::Tool(
                            self.task_tool.description().to_string(),
                            self.task_tool.arguments(),
                        ))
                        .into_self(),
                ]);
            }
        }
        if let Some(ref p) = self.parent {
            p.query(select).await
        } else {
            Ok(Vec::new())
        }
    }

    async fn spawn(&self, tasks: Vec<fae_agent::Task>) -> anyhow::Result<()> {
        let (ts, ptasks): (Vec<_>, Vec<_>) =
            tasks.into_iter().partition(|t| Self::is_task_tool_task(t));
        if !ptasks.is_empty() {
            if let Some(ref p) = self.parent {
                p.spawn(ptasks).await?;
            } else {
                return anyhow::anyhow!("[TaskRuntime] spawn failed, no parent").err();
            }
        }
        for task in ts {
            let result = self.exec_tool(task).await?;
            if let Err(e) = self.task_result_channel.send(result).await {
                return anyhow::anyhow!("[TaskRuntime] spawn failed, send result error: {:?}", e)
                    .err();
            }
        }
        Ok(())
    }

    async fn execute(&self, task: fae_agent::Task) -> anyhow::Result<TaskResult> {
        if Self::is_task_tool_task(&task) {
            return self.exec_tool(task).await;
        }
        if let Some(ref p) = self.parent {
            p.execute(task).await
        } else {
            anyhow::anyhow!("[TaskRuntime] execute failed, no parent").err()
        }
    }
}

// -------------- AgentTaskStore 的内存实现 --------------
#[derive(Debug)]
struct AgentTaskStoreMap {
    tasks: HashMap<String, AgentTask>,
    session: HashMap<String, Vec<String>>,
}
#[derive(Debug)]
pub struct DefaultAgentTaskStore {
    pub map: RwLock<AgentTaskStoreMap>,
}

#[async_trait::async_trait]
impl AgentTaskStore for DefaultAgentTaskStore {
    async fn push_task(&self, task: AgentTask) {
        let mut map = self.map.write().await;
        if let Some(tasks) = map.session.get_mut(task.get_author_session_id()) {
            tasks.push(task.get_task_id().to_string());
        } else {
            map.session.insert(
                task.get_author_session_id().to_string(),
                vec![task.get_task_id().to_string()],
            );
        }
        map.tasks.insert(task.get_task_id().to_string(), task);
    }

    async fn update_status_executing(&self, task_id: &str) -> anyhow::Result<()> {
        let mut map = self.map.write().await;
        if let Some(task) = map.tasks.get_mut(task_id) {
            task.status = AgentTaskStatus::EXECUTING.to_string();
            task.update_timestamp();
        } else {
            return anyhow::anyhow!("task not found: {}", task_id).err();
        }
        Ok(())
    }

    async fn complete_task(
        &self,
        task_id: &str,
        status: String,
        result: String,
        ext: Option<Box<dyn Any + Send + Sync + 'static>>,
    ) -> anyhow::Result<AgentTasks> {
        let mut map = self.map.write().await;
        let mut session_id = String::new();
        if let Some(task) = map.tasks.get_mut(task_id) {
            task.status = status;
            task.result = result;
            task.update_timestamp();
            if let Some(e) = ext {
                task.set_ext(e);
            }
            session_id = task.get_author_session_id().to_string();
        } else {
            return anyhow::anyhow!("[TaskStore]::complete_task task not found: {}", task_id).err();
        }
        let ids = if let Some(s) = map.session.get(&session_id) {
            s
        } else {
            return anyhow::anyhow!(
                "[TaskStore]::complete_task session not found: {}",
                session_id
            )
            .err();
        };
        for i in ids {
            if let Some(t) = map.tasks.get(i) {
                if !t.status_is_complete() {
                    return Ok(AgentTasks::default());
                }
            } else {
                return anyhow::anyhow!("[TaskStore]::complete_task task not found: {}", i).err();
            }
        }
        let ids = map.session.remove(&session_id).unwrap();
        //全部任务均已完成
        let mut agentasks = AgentTasks::default();
        for i in ids {
            if let Some(t) = map.tasks.remove(i.as_str()) {
                agentasks.push(t);
            }
        }
        Ok(agentasks)
    }
}

impl Default for DefaultAgentTaskStore {
    fn default() -> Self {
        Self {
            map: RwLock::new(AgentTaskStoreMap {
                tasks: HashMap::default(),
                session: HashMap::default(),
            }),
        }
    }
}

// -------------- AgentRuntime 的默认实现 --------------
impl Default for AgentRuntime {
    fn default() -> Self {
        Self::new(DefaultAgentTaskStore::default())
    }
}
