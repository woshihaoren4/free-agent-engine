use crate::{IdenInfo, Tool};
use fae_agent::{AgentInfo, AgentTask, AgentTaskExt, AgentTaskStatus, Env, EnvEvent, Environment, Select, TaskResult, TaskType, Thing, ThingItem, ThingSelect, ToolRequest, ToolResponse};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::RwLock;
use wd_tools::channel::Channel;
use wd_tools::PFErr;
use crate::tools::{AgentTaskTool, AGENT_TASK_TOOL_NAME};

const AGENT_TASK_RUNTIME_ID: &str = "AGENT_TASK_RUNTIME_ID";

#[async_trait::async_trait]
pub trait AgentTaskStore: Debug {
    async fn push_task(&self, task: AgentTask);
    async fn load_tasks(&self,task_id:&str) -> anyhow::Result<Vec<AgentTask>>;
    async fn get_task_author_info(&self,task_id:&str) -> anyhow::Result<AgentInfo>;
}

#[derive(Debug)]
pub struct AgentRuntime {
    pub task_store: Arc<dyn AgentTaskStore+Send+Sync+'static>,
    pub task_tool: AgentTaskTool,
    pub channel: Channel<AgentTaskExt>,
    pub spawn_channel: Channel<TaskResult>,
    parent: Option<Env>,
}

impl AgentRuntime {
    pub fn new(store:impl AgentTaskStore+Send+Sync+'static) -> Self {
        let chan = Channel::with_cap(100);
        let spawn_chan = Channel::with_cap(100);
        let tool = AgentTaskTool::new(chan.clone());
        Self {
            task_store: Arc::new(store),
            task_tool: tool,
            spawn_channel: spawn_chan,
            parent: None,
            channel: chan,
        }
    }
    pub async fn exec_tool(&self, mut task: fae_agent::Task) -> anyhow::Result<TaskResult> {
        if let Some(req) = task.into_inner::<ToolRequest>() {
            let iden = IdenInfo::new(
                task.id.clone(),
                task.agent_id.clone(),
                task.user_id.clone(),
                task.ctx.clone(),
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
        task.get_type() == &TaskType::Tool
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
            let spawn_recv = self.spawn_channel.recv();

            tokio::select! {
                res = recv => {
                    let mut task = res?;
                    self.task_store.push_task(task.task.clone()).await;
                    match &mut task.task.content {
                        AgentTaskStatus::Create(e) => {
                            return Ok(EnvEvent::Agent(task));
                        }
                        AgentTaskStatus::Executing(_) => {}
                        AgentTaskStatus::Completed(output) => {
                            let task_author = self.task_store.get_task_author_info(&task.task.task_id).await?;
                            let author_agent_id = task_author.agent_id.clone();
                            output.task_author = task_author;
                            let task = TaskResult::success(task.task.task_id.clone(), author_agent_id).set_data(task);
                            return Ok(EnvEvent::TaskResult(task));
                        }
                        AgentTaskStatus::Failed(output) => {
                            let task_author = self.task_store.get_task_author_info(&task.task.task_id).await?;
                            let author_agent_id = task_author.agent_id.clone();
                            output.task_author = task_author;
                            let task = TaskResult::success(task.task.task_id.clone(), author_agent_id).set_data(task);
                            return Ok(EnvEvent::TaskResult(task));
                        }
                    }
                }
                res = spawn_recv =>{
                    let result = res?;
                    return Ok(EnvEvent::TaskResult(result));
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
        }else if let ThingSelect::AgenTask(id) = &select.select {
            let tasks = self.task_store.load_tasks(id).await?;
            let tasks = tasks.into_iter().map(|x|x.content).collect::<Vec<_>>();
            let mut source = Thing::new(self.id().to_string());
            let thing = source.add_item(ThingItem::AgenTask(tasks)).into_self();
            return Ok(vec![thing]);
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
            if let Err(e) = self.spawn_channel.send(result).await{
                return anyhow::anyhow!("[TaskRuntime] spawn failed, send result error: {:?}", e).err();
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
pub struct DefaultAgentTaskStore{
    pub map:RwLock<HashMap<String,Vec<AgentTask>>>,
}

#[async_trait::async_trait]
impl AgentTaskStore for DefaultAgentTaskStore {
    async fn push_task(&self, task: AgentTask) {
        let mut write = self.map.write().await;
        if let Some(s) = write.get_mut(&task.task_id) {
            s.push(task);
        } else {
            write.insert(task.task_id.clone(), vec![task]);
        }
    }

    async fn load_tasks(&self, task_id: &str) -> anyhow::Result<Vec<AgentTask>> {
        let read = self.map.read().await;
        if let Some(s) = read.get(task_id) {
            Ok(s.clone())
        }else{
            return anyhow::anyhow!("[TaskRuntime] load_tasks failed, task_id not found").err();
        }
    }

    async fn get_task_author_info(&self, task_id: &str) -> anyhow::Result<AgentInfo> {
        let read = self.map.read().await;
        if let Some(s) = read.get(task_id) {
            for i in s.iter().rev(){
                if let AgentTaskStatus::Create(ref t) = i.content{
                    return Ok(t.from_agent.clone());
                }
            }
        }
        return anyhow::anyhow!("[TaskRuntime] get_task_author_info failed, task_id not found").err();
    }
}

impl Default for DefaultAgentTaskStore {
    fn default() -> Self {
        Self{
            map: RwLock::new(HashMap::new()),
        }
    }
}

// -------------- AgentRuntime 的默认实现 --------------
impl Default for AgentRuntime {
    fn default() -> Self {
        Self::new(DefaultAgentTaskStore::default())
    }
}
