use std::sync::atomic::Ordering;

use serde_json::Value;

use crate::CommonSession;

#[derive(Debug)]
pub struct WorkflowEnv {
    pub workflow_id: String,
    pub input: Value,
    pub(crate) session: CommonSession,
}

impl WorkflowEnv {
    pub fn new(workflow_id: impl Into<String>, input: Value) -> (Self, CommonSession) {
        let session = CommonSession::new();
        (
            Self {
                workflow_id: workflow_id.into(),
                input,
                session: session.clone(),
            },
            session,
        )
    }

    pub fn session(&self) -> CommonSession {
        self.session.clone()
    }

    #[doc(hidden)]
    pub fn defer_context_completion(&self) {
        self.session
            .completion
            .complete_context
            .store(false, Ordering::Release);
    }

    pub(crate) fn completes_context(&self) -> bool {
        self.session
            .completion
            .complete_context
            .load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{Session, SessionEvent, SessionEventData};

    use super::*;

    #[tokio::test]
    async fn result_does_not_consume_terminal_event() {
        let session = CommonSession::new();
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
