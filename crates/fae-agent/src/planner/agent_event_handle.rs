use crate::define::{Msg, OutMsgOnce, ReceiverMessageStream, SenderMessageStream};
use crate::{Agent, AgentConfig, Command, Env, EnvEvent, Memory, Message, Planning, Session, SessionCtl, SessionEventLayer, SessionMetadata, SessionMD, TaskResult};
use std::marker::PhantomData;
use std::sync::Arc;
use wd_tools::PFErr;

#[async_trait::async_trait]
pub trait AgentEventHandle<S, In, Out, P>: Sync
where
    S: SessionMetadata + Send + Sync + 'static,
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    P: Planning + Send + 'static,
{
    fn id(&self) -> String;
    fn desc(&self) -> String {
        String::new()
    }
    async fn on_info(&self) -> Arc<dyn AgentConfig+Send+'static>;
    /// 处理无事件
    async fn on_memory(&self) -> Arc<dyn Memory + Send + 'static>;
    /// 处理会话事件
    async fn on_session_ctl(&self) -> Arc<dyn SessionCtl + Send + 'static>;

    async fn on_none(&self) {}
    /// 处理会话调用事件
    async fn on_session_call(
        &self,
        _env: Env,
        meta: &mut S,
        input: In,
        _output: OutMsgOnce<Out>,
    ) -> anyhow::Result<P> {
        anyhow::anyhow!(
            "[AgentEventExt::{}] not support on_session_call, session_id:{:?}",
            self.id(),
            meta.id()
        )
        .err()
    }
    /// 处理流式会话调用
    async fn on_session_call_stream(
        &self,
        _env: Env,
        meta: &mut S,
        input: In,
        output: SenderMessageStream<Out>,
    ) -> anyhow::Result<P> {
        anyhow::anyhow!(
            "[AgentEventExt::{}] not support on_session_call_stream, session_id:{:?}",
            self.id(),
            meta.id()
        )
        .err()
    }
    /// 处理流式输入，单次输出
    async fn on_session_stream_call(
        &self,
        _env: Env,
        meta: &mut S,
        input: ReceiverMessageStream<In>,
        output: OutMsgOnce<Out>,
    ) -> anyhow::Result<P> {
        anyhow::anyhow!(
            "[AgentEventExt::{}] not support on_session_stream_call, session_id:{:?}",
            self.id(),
            meta.id()
        )
        .err()
    }
    /// 输入输出双流式
    async fn on_session_stream(
        &self,
        env: Env,
        meta: &mut S,
        input: ReceiverMessageStream<In>,
        output: SenderMessageStream<Out>,
    ) -> anyhow::Result<P> {
        anyhow::anyhow!(
            "[AgentEventExt::{}] not support on_session_stream, session_id:{:?}",
            self.id(),
            meta.id()
        )
        .err()
    }
    /// 任务结果回调
    async fn on_task_result_callback(&self, env: Env, result: TaskResult) -> anyhow::Result<()> {
        wd_log::log_info_ln!(
            "[AgentEventExt::{}] on_task_result_callback: {:?}",
            self.id(),
            result
        );
        Ok(())
    }
    /// 指令
    async fn on_command(&self, env: Env, command: String) -> anyhow::Result<()> {
        wd_log::log_info_ln!(
            "[AgentEventExt::{}] on_command_callback: {:?}",
            self.id(),
            command
        );
        Ok(())
    }
    /// 心跳
    async fn on_heartbeat(&self, env: Env, heartbeat: String) -> anyhow::Result<()> {
        wd_log::log_info_ln!("[AgentEventExt::{}] on_heartbeat", self.id());
        Ok(())
    }
    /// 处理退出事件
    async fn exit(&self);
}

pub struct AgentEventHandleImpl<E, S, In, Out, P> {
    agent_event_ext: Arc<E>,
    _s: PhantomData<S>,
    _in: PhantomData<In>,
    _out: PhantomData<Out>,
    _p: PhantomData<P>,
}

impl<E, S, In, Out, P> AgentEventHandleImpl<E, S, In, Out, P> {
    pub fn new(agent_event_ext: Arc<E>) -> Self {
        Self {
            agent_event_ext,
            _s: PhantomData,
            _in: PhantomData,
            _out: PhantomData,
            _p: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<E, S, In, Out, P> Agent for AgentEventHandleImpl<E, S, In, Out, P>
where
    E: AgentEventHandle<S, In, Out, P> + Send + 'static,
    S: SessionMetadata + Send + Sync + 'static,
    In: Send + Sync + 'static + Message,
    Out: Send + Sync + 'static + Message,
    P: Planning + Send + 'static,
{
    fn id(&self) -> String {
        self.agent_event_ext.id()
    }

    fn desc(&self) -> String {
        self.agent_event_ext.desc()
    }

    async fn on_info(&self) -> Arc<dyn AgentConfig + Send + 'static> {
        self.agent_event_ext.on_info().await
    }

    async fn on_memory(&self) -> Arc<dyn Memory + Send + 'static> {
        self.agent_event_ext.on_memory().await
    }

    /// 处理会话事件
    async fn on_session_ctl(&self) -> Arc<dyn SessionCtl + Send + 'static> {
        self.agent_event_ext.on_session_ctl().await
    }

    async fn on_env(&self, env: Env, event: EnvEvent) -> anyhow::Result<()> {
        match event {
            EnvEvent::None => {
                self.agent_event_ext.on_none().await;
                Ok(())
            }
            EnvEvent::TaskResult(result) => {
                self.agent_event_ext
                    .on_task_result_callback(env, result)
                    .await
            }
            EnvEvent::Heartbeat(s) => self.agent_event_ext.on_heartbeat(env, s).await,
        }
    }

    async fn on_session(
        &self,
        env: Env,
        meta: SessionMD,
    ) -> anyhow::Result<Box<dyn Session + Send + 'static>> {
        Ok(Box::new(SessionEventLayer::new(
            env,
            meta,
            self.agent_event_ext.clone(),
        )?))
    }

    async fn on_command(&self, env: Env, cmd: Command) -> anyhow::Result<()> {
        match cmd {
            Command::None => {
                self.agent_event_ext.on_none().await;
                Ok(())
            }
            Command::SystemExit => {
                self.agent_event_ext.exit().await;
                Ok(())
            }
            Command::CustomCommand(cmd) => self.agent_event_ext.on_command(env, cmd).await,
        }
    }

    async fn exit(&self) {
        self.agent_event_ext.exit().await;
    }
}
