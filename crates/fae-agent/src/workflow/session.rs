use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::Notify;

use crate::{Session, SessionEvent, SessionEventChannel, SessionEventData, WorkflowMetadata};

#[derive(Debug)]
pub struct WorkflowEnv {
    pub meta: WorkflowMetadata,
    pub(crate) session: WorkflowSession,
    pub(crate) complete_context: bool,
}

impl WorkflowEnv {
    pub fn new(meta: WorkflowMetadata) -> (Self, WorkflowSession) {
        let session = WorkflowSession::new();
        (
            Self {
                meta,
                session: session.clone(),
                complete_context: true,
            },
            session,
        )
    }

    pub fn session(&self) -> WorkflowSession {
        self.session.clone()
    }

    pub fn defer_context_completion(mut self) -> Self {
        self.complete_context = false;
        self
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowSession {
    channel: SessionEventChannel,
    completion: Arc<WorkflowCompletion>,
}

#[derive(Debug, Default)]
struct WorkflowCompletion {
    result: Mutex<Option<Result<Value, String>>>,
    notify: Notify,
}

impl WorkflowSession {
    fn new() -> Self {
        Self {
            channel: SessionEventChannel::new(),
            completion: Arc::new(WorkflowCompletion::default()),
        }
    }

    pub(crate) fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        let terminal = event.is_terminal().then(|| match &event.data {
            SessionEventData::NodeCompleted { output, .. } => Ok(output.clone()),
            SessionEventData::Failed { error } => Err(error.clone()),
            _ => unreachable!("terminal workflow event has an unsupported payload"),
        });
        self.channel.emit(event)?;
        if let Some(result) = terminal {
            self.completion.complete(result);
        }
        Ok(())
    }

    pub async fn result(&self) -> anyhow::Result<Value> {
        loop {
            let notified = self.completion.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if let Some(result) = self.completion.result.lock().unwrap().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            notified.await;
        }
    }
}

impl WorkflowCompletion {
    fn complete(&self, result: Result<Value, String>) {
        let mut completion = self.result.lock().unwrap();
        if completion.is_some() {
            return;
        }
        *completion = Some(result);
        drop(completion);
        self.notify.notify_waiters();
    }
}

#[async_trait::async_trait]
impl Session<(), SessionEvent> for WorkflowSession {
    async fn call(&self, _input: ()) -> anyhow::Result<()> {
        anyhow::bail!("workflow session does not accept input")
    }

    async fn answer(&self) -> anyhow::Result<Option<SessionEvent>> {
        Ok(self.channel.answer().await)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn result_does_not_consume_terminal_event() {
        let session = WorkflowSession::new();
        session
            .emit(SessionEvent::workflow(
                "workflow",
                "end",
                SessionEventData::NodeCompleted {
                    output: json!({"done": true}),
                    finished: true,
                },
            ))
            .unwrap();

        assert_eq!(session.result().await.unwrap(), json!({"done": true}));
        assert!(session.answer().await.unwrap().unwrap().is_terminal());
    }
}
