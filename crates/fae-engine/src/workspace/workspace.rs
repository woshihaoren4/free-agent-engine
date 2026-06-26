use crate::AgentCtl;
use fae_agent::{
    AgentConfig, AgentTaskStatus, Env, EnvEvent, Environment,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

// 0:初始化，1:运行中，2:已停止
#[derive(Debug, Clone, Default)]
pub struct WorkspaceStatus(Arc<AtomicU8>);

impl WorkspaceStatus {
    pub fn set_running(&self) {
        self.0.store(1, Ordering::Relaxed);
    }
    pub fn is_running(&self) -> bool {
        self.0.load(Ordering::Relaxed) == 1
    }
    pub fn set_stopped(&self) {
        self.0.store(2, Ordering::Relaxed);
    }
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed) == 2
    }
}

#[derive(Clone)]
pub struct Workspace {
    pub(crate) status: WorkspaceStatus,
    pub(crate) name: String,
    pub(crate) loader: Arc<dyn AgentCtl + Send + 'static>,
    pub(crate) env: Env,
}

impl Workspace {
    pub(crate) async fn push_event(&self, agent_id: String, event: EnvEvent) {
        let agent = match self.get_agent(agent_id.as_str()).await {
            Ok(agent) => agent,
            Err(e) => {
                wd_log::log_error_ln!(
                    "[Workspace::{}] load agent: {} failed: {:?}",
                    self.name,
                    agent_id,
                    e
                );
                return;
            }
        };
        if let Err(e) = agent.on_env(self.env.clone(), event).await {
            wd_log::log_error_ln!("[Workspace::{}] on_env failed: {:?}", self.name, e);
        }
    }
    //启动工作空间，监听环境变化
    pub(crate) fn start_watch_env(&self) {
        let this = self.clone();
        this.status.set_running();
        tokio::spawn(async move {
            while this.status.is_running() {
                let event = this.env.watch().await;
                let event = match event {
                    Ok(event) => event,
                    Err(e) => {
                        wd_log::log_error_ln!(
                            "[Workspace::{}] watch env failed: {:?}",
                            this.name,
                            e
                        );
                        continue;
                    }
                };
                if event.is_none() {
                    // 没有事件，等待1ms
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    continue;
                }
                //分发事件给智能体
                match event {
                    EnvEvent::TaskResult(ref result) => {
                        // 分发任务执行结果给智能体
                        let aid = result.agent_id.clone();
                        this.push_event(aid, event).await;
                    }
                    EnvEvent::Timed(ref task) => {
                        let aid = task.agent_id.clone();
                        this.push_event(aid, event).await;
                    }
                    EnvEvent::Agent(ref agent) => match agent.first_task_status() {
                        AgentTaskStatus::EXECUTING => {}
                        _ => {
                            let aid = agent.get_agent_id();
                            this.push_event(aid.to_string(), event).await;
                        }
                    },
                    _ => {
                        // 其他事件，不处理
                        wd_log::log_info_ln!(
                            "[Workspace::{}] ignore event: {:?}",
                            this.name,
                            &event
                        );
                    }
                }
            }
            this.status.set_stopped();
        });
    }

    pub fn get_env(&self) -> Env {
        self.env.clone()
    }

    pub async fn exit(&self) {
        self.status.set_stopped();
        if let Err(e) = self.loader.exit().await {
            wd_log::log_error_ln!("[Workspace::{}] exit failed: {:?}", self.name, e);
        }
    }
}
