use std::any::Any;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_stream::Stream;
use wd_tools::channel::Channel;
use wd_tools::PFErr;
use crate::Error;


#[derive(Default,Debug)]
pub enum EnvEvent{
    #[default]
    None,
    Heartbeat,
    KV(String,Value),
    Custom(String),
    Any(Box<dyn Any + Send>),
}

#[async_trait::async_trait]
pub trait Env:Sync{
    async fn watch(&self) -> Channel<EnvEvent>;
    async fn push(&self,event:EnvEvent)-> anyhow::Result<()>;
}

#[derive(Default,Debug)]
pub enum Command{
    #[default]
    None,
    Reset(String),
    Set(String,Value),
    ClearSession,
    Stop,
    Custom(String),
    Any(Box<dyn Any + Send>),
}

#[derive(Default,Debug)]
pub enum Message{
    #[default]
    None,
    Text(String),
    Binary(Vec<u8>),
    Json(Value),
    Command(Command),
    Any(Box<dyn Any + Send>),
}

#[async_trait::async_trait]
pub trait Session:Sync{
    async fn call(&self,_input:Message)->anyhow::Result<Message,Error>{
        Error::NoSupport("Session.call".into()).err()
    }

    async fn call_stream(&self,_input:Message)->anyhow::Result<Box<dyn Stream<Item=Message>+Send>, Error>{
        Error::NoSupport("Session.call_stream".into()).err()
    }

    async fn stream_call(&self,_input:Box<dyn Stream<Item=Message>+Send>)->anyhow::Result<Vec<Message>, Error>{
        Error::NoSupport("Session.stream_call".into()).err()
    }

    async fn stream(&self,_input:Box<dyn Stream<Item=Message>+Send>)->anyhow::Result<Box<dyn Stream<Item=Message>+Send>, Error>{
        Error::NoSupport("Session.stream".into()).err()
    }
}


#[async_trait::async_trait]
pub trait Agent{
    async fn on_env(&self,event:EnvEvent)-> anyhow::Result<()>;
    async fn on_session(&self,session_id:&str)-> anyhow::Result<Arc<dyn Session+Send+'static>>;
    async fn on_option(&self)-> Result<(), Error>;
}

#[cfg(test)]
mod tests {

}