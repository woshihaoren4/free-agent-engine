use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use fae_agent::{AgentRef, Env, EnvEvent, Environment};
use crate::engine::AgentLoader;

// 0:初始化，1:运行中，2:已停止
#[derive(Debug,Clone,Default)]
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
    pub(crate) loader: Arc<dyn AgentLoader + Send + 'static>,
    pub(crate) env : Env,
}

impl Workspace {
    //启动工作空间，监听环境变化
    pub fn start_watch_env(&self) {
        let this = self.clone();
        this.status.set_running();
        tokio::spawn(async move {
            while this.status.is_running() {
                let event = this.env.watch().await;
                if event.is_none() {
                    // 没有事件，等待1ms
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    continue;
                }
                //分发事件给智能体
                match event{
                    EnvEvent::TaskResult(ref result) => {
                        // 分发任务执行结果给智能体
                        let agent = match this.get_agent(result.agent_id.as_str()).await {
                            Ok(agent) => agent,
                            Err(e) => {
                                wd_log::log_error_ln!("[Workspace::{}] load agent {} failed: {:?}",this.name,result.agent_id,e);
                                continue;
                            }
                        };
                        if let Err(e) = agent.on_env(this.env.clone(),event).await {
                            wd_log::log_error_ln!("[Workspace::{}] on_env failed: {:?}",this.name,e);
                        }
                    }
                    _ => {
                        // 其他事件，不处理
                        wd_log::log_info_ln!("[Workspace::{}] ignore event: {:?}",this.name,&event);
                    }
                }
            }
            this.status.set_stopped();
        });
    }
    pub async fn get_agent(&self, agent_id:&str) -> anyhow::Result<AgentRef> {
        self.loader.load(agent_id).await
    }
    pub fn get_env(&self) -> Env {
        self.env.clone()
    }
    pub async fn exit(&self) {
        self.status.set_stopped();
        if let Err(e) = self.loader.exit().await {
            wd_log::log_error_ln!("[Workspace::{}] exit failed: {:?}",self.name,e);
        }
    }
}