use crate::Workspace;
use fae_agent::{
    Message, Session, SessionCall, SessionCallStream, SessionMetadata, SessionStream,
    SessionStreamCall,
};

impl Workspace {
    // 创建一个原始会话
    pub async fn session<T: Into<SessionMetadata>>(
        &self,
        agent: &str,
        meta: T,
    ) -> anyhow::Result<Box<dyn Session + Send + 'static>> {
        let agent = self.get_agent(agent).await?;
        agent.on_session(self.env.clone(), meta.into()).await
    }

    // 创建一个封装会话 (call_stream)
    pub async fn session_call_stream<T: Into<SessionMetadata>, In, Out>(
        &self,
        agent: &str,
        meta: T,
    ) -> anyhow::Result<Box<dyn SessionCallStream<In, Out> + Send + 'static>>
    where
        In: Send + Sync + 'static + Message,
        Out: Send + Sync + 'static + Message,
    {
        let session = self.session(agent, meta).await?;
        Ok(Box::new(session))
    }

    // 创建一个封装会话 (call)
    pub async fn session_call<T: Into<SessionMetadata>, In, Out>(
        &self,
        agent: &str,
        meta: T,
    ) -> anyhow::Result<Box<dyn SessionCall<In, Out> + Send + 'static>>
    where
        In: Send + Sync + 'static + Message,
        Out: Send + Sync + 'static + Message,
    {
        let session = self.session(agent, meta).await?;
        Ok(Box::new(session))
    }

    // 创建一个封装会话 (stream_call)
    pub async fn session_stream_call<T: Into<SessionMetadata>, In, Out>(
        &self,
        agent: &str,
        meta: T,
    ) -> anyhow::Result<Box<dyn SessionStreamCall<In, Out> + Send + 'static>>
    where
        In: Send + Sync + 'static + Message,
        Out: Send + Sync + 'static + Message,
    {
        let session = self.session(agent, meta).await?;
        Ok(Box::new(session))
    }

    // 创建一个封装会话 (stream)
    pub async fn session_stream<T: Into<SessionMetadata>, In, Out>(
        &self,
        agent: &str,
        meta: T,
    ) -> anyhow::Result<Box<dyn SessionStream<In, Out> + Send + 'static>>
    where
        In: Send + Sync + 'static + Message,
        Out: Send + Sync + 'static + Message,
    {
        let session = self.session(agent, meta).await?;
        Ok(Box::new(session))
    }
}
