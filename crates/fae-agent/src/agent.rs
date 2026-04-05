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
    Heartbeat(String),
    KV(String,Value),
    Custom(String),
    Any(Box<dyn Any + Send + Sync + 'static>),
}

#[async_trait::async_trait]
pub trait EnvWatch:Sync{
    async fn watch(&self) -> Channel<EnvEvent>;
}

#[derive(Default,Debug)]
pub enum Message{
    #[default]
    None,
    Text(String),
    Binary(Vec<u8>),
    Value(Value),
    Command(Command),
    Any(Box<dyn Any + Send + Sync + 'static>),
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

#[derive(Default,Debug)]
pub enum Command{
    #[default]
    None,
    SystemReset,
    SystemExit,
    UserCustomCommand(String),
    Any(Box<dyn Any + Send + Sync + 'static>),
}

#[derive(Default,Debug)]
pub enum Event{
    #[default]
    None,
    Session(Message),
    EnvEvent(EnvEvent),
    TaskOver(Task)
}

#[derive(Default,Debug)]
pub enum Task{
    #[default]
    None,
    Module(String),
    Tool(String),
    Agent(String),
    Skill(String),
    Custom(String),
    Output(String),
    Error(String),
    Over,
    Any(Box<dyn Any + Send + Sync + 'static>),
}

#[derive(Default,Debug)]
pub struct Context{

}

#[async_trait::async_trait]
pub trait AgentPlanning{
    async fn start(&self,event:&Event)-> anyhow::Result<Arc<dyn Any + Send + Sync + 'static>>;
    async fn next_step(&self,ctx:Arc<dyn Any + Send + Sync + 'static>,event:Event)-> anyhow::Result<Vec<Task>>;
    async fn over(&self,ctx:Arc<dyn Any + Send + Sync + 'static>)->anyhow::Result<()>;
}

#[async_trait::async_trait]
pub trait Agent {
    async fn on_env(&self, event: &EnvEvent) -> anyhow::Result<()>;
    async fn on_session(&self) -> anyhow::Result<Box<dyn Session + Send + Sync + 'static>>;
    async fn on_command(&self, cmd: Command) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {

}