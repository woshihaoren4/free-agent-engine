use std::sync::atomic::Ordering;

use serde_json::Value;

use crate::{CommonSession, DEFAULT_USER_ID};

#[derive(Debug)]
pub struct WorkflowEnv {
    pub workflow_id: String,
    pub input: Value,
    pub user_id: String,
    pub(crate) session: CommonSession,
}

impl WorkflowEnv {
    pub fn new(workflow_id: impl Into<String>, input: Value) -> (Self, CommonSession) {
        Self::new_with_user_id(workflow_id, input, DEFAULT_USER_ID)
    }

    pub fn new_with_user_id(
        workflow_id: impl Into<String>,
        input: Value,
        user_id: impl Into<String>,
    ) -> (Self, CommonSession) {
        let user_id = user_id.into();
        let session = CommonSession::new_with_user_id(user_id.clone());
        (
            Self {
                workflow_id: workflow_id.into(),
                input,
                user_id,
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

    #[test]
    fn workflow_env_and_session_default_to_master_user() {
        let (env, session) = WorkflowEnv::new("workflow", Value::Null);

        assert_eq!(env.user_id, DEFAULT_USER_ID);
        assert_eq!(session.user_id(), DEFAULT_USER_ID);
    }

    #[test]
    fn workflow_env_and_session_accept_explicit_user() {
        let (env, session) = WorkflowEnv::new_with_user_id("workflow", Value::Null, "alice");

        assert_eq!(env.user_id, "alice");
        assert_eq!(session.user_id(), "alice");
    }

    #[test]
    fn workflow_agent_session_inherits_workflow_user() {
        let (_, workflow_session) = WorkflowEnv::new_with_user_id("workflow", Value::Null, "alice");
        let (env, agent_session) = crate::SingleAgentEnv::new_with_session(
            crate::SingleAgentSource::AgentId("agent".to_string()),
            "input",
            workflow_session,
            "workflow",
            "agent-node",
        );

        assert_eq!(env.user_id, "alice");
        assert_eq!(agent_session.user_id(), "alice");
    }

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
