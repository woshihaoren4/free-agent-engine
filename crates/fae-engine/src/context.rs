use crate::Engine;
use fae_agent::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;

const STACK_CAPACITY: usize = 16_384;
#[derive(Debug, Default)]
struct StackStore {
    entries: Vec<ContextStack>,
    parents: HashMap<String, Option<String>>,
    sequence: u64,
    starts: HashMap<(String, String), (u64, std::time::Instant)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn root() -> Ctx {
        crate::EngineBuilder::new()
            .build()
            .await
            .ctx_with_scope(ContextScope::root("alice"))
            .unwrap()
    }

    #[tokio::test]
    async fn scope_binding_clone_fork_and_terminal_ownership() {
        let ctx = root().await;
        assert!(ctx.bind_scope(ContextScope::root("bob")).is_err());
        assert!(ctx.set_state(ContextState::succeeded(1u8)).is_err());
        assert_eq!(
            ctx.set_state(ContextState::Running).unwrap(),
            ContextStateUpdate::Applied
        );
        assert_eq!(
            ctx.clone().set_state(ContextState::Running).unwrap(),
            ContextStateUpdate::Unchanged
        );
        let child = ctx
            .fork(ContextChildSpec {
                agent_id: Some("worker".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(child.scope().unwrap().user.user_id, "alice");
        assert_eq!(
            child.scope().unwrap().parent_scope_id.as_deref(),
            Some(ctx.scope().unwrap().scope_id.as_str())
        );
        child.set_state(ContextState::Running).unwrap();
        child.set_state(ContextState::succeeded(42u32)).unwrap();
        assert!(matches!(&*ctx.state().current(), ContextState::Running));
        ctx.set_state(ContextState::cancelled("stop tree")).unwrap();
        assert_eq!(child.result::<u32>().await.unwrap(), 42);
        assert!(matches!(
            &*child.state().current(),
            ContextState::Succeeded { .. }
        ));
        assert!(ctx.fork(ContextChildSpec::default()).is_err());
        assert!(ctx.set_state(ContextState::failed("late failure")).is_err());
        assert_eq!(
            ctx.result::<()>().await.unwrap_err().to_string(),
            "stop tree"
        );
    }

    #[tokio::test]
    async fn results_preserve_type_and_support_early_and_late_waiters() {
        let ctx = root().await;
        ctx.set_state(ContextState::Running).unwrap();
        let early = ctx.clone();
        let waiter = tokio::spawn(async move { early.result::<Vec<u8>>().await });
        ctx.set_state(ContextState::succeeded(vec![1u8, 2]))
            .unwrap();
        assert!(
            ctx.result::<String>()
                .await
                .unwrap_err()
                .to_string()
                .contains("type does not match")
        );
        assert_eq!(waiter.await.unwrap().unwrap(), vec![1, 2]);
        assert!(
            ctx.result::<Vec<u8>>()
                .await
                .unwrap_err()
                .to_string()
                .contains("already been consumed")
        );
        assert!(matches!(
            &*ctx.state().current(),
            ContextState::Succeeded { .. }
        ));
        assert!(matches!(
            &*ctx.terminal().await.unwrap(),
            ContextState::Succeeded { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_terminal_writes_have_one_winner_and_one_consumer() {
        let ctx = root().await;
        ctx.set_state(ContextState::Running).unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(4));
        let mut writers = Vec::new();
        for state in [
            ContextState::succeeded(7u8),
            ContextState::failed("failed"),
            ContextState::cancelled("cancelled"),
        ] {
            let ctx = ctx.clone();
            let barrier = barrier.clone();
            writers.push(tokio::spawn(async move {
                barrier.wait().await;
                ctx.set_state(state).is_ok()
            }));
        }
        barrier.wait().await;
        let mut winners = 0;
        for writer in writers {
            winners += usize::from(writer.await.unwrap());
        }
        assert_eq!(winners, 1);
        assert!(ctx.state().current().is_terminal());

        let success = root().await;
        success.set_state(ContextState::Running).unwrap();
        success.set_state(ContextState::succeeded(9u8)).unwrap();
        let left = success.clone();
        let right = success.clone();
        let (left, right) = tokio::join!(left.result::<u8>(), right.result::<u8>());
        assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fork_racing_parent_cancellation_cannot_escape_tree() {
        for _ in 0..64 {
            let ctx = root().await;
            ctx.set_state(ContextState::Running).unwrap();
            let fork_ctx = ctx.clone();
            let fork = tokio::spawn(async move { fork_ctx.fork(ContextChildSpec::default()) });
            ctx.set_state(ContextState::cancelled("tree reason"))
                .unwrap();
            if let Ok(child) = fork.await.unwrap() {
                assert_eq!(
                    child.result::<()>().await.unwrap_err().to_string(),
                    "tree reason"
                );
            }
        }
        let ctx = root().await;
        let child = ctx.fork(ContextChildSpec::default()).unwrap();
        let grandchild = child.fork(ContextChildSpec::default()).unwrap();
        ctx.set_state(ContextState::failed("root failed")).unwrap();
        assert_eq!(
            grandchild.result::<()>().await.unwrap_err().to_string(),
            "root failed"
        );
    }

    #[tokio::test]
    async fn child_session_cannot_replace_inherited_user_or_parent_ownership() {
        let ctx = root().await;
        let (_, wrong_session) =
            WorkflowEnv::new_with_user_id("child", serde_json::Value::Null, "bob");
        assert!(
            ctx.fork(ContextChildSpec {
                session: Some(ContextSession {
                    session_id: "child-session".into(),
                    session: wrong_session
                }),
                ..Default::default()
            })
            .is_err()
        );
        let task = ctx
            .fork(ContextChildSpec {
                agent_id: Some("worker".into()),
                turn_id: Some(2),
                ..Default::default()
            })
            .unwrap();
        let execution = task
            .fork(ContextChildSpec {
                execution: true,
                workflow_id: Some("flow".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(execution.scope().unwrap().user.user_id, "alice");
        assert!(execution.scope().unwrap().agent_id.is_none());
        assert!(execution.scope().unwrap().turn_id.is_none());
        assert_eq!(task.scope().unwrap().agent_id.as_deref(), Some("worker"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn structured_stacks_preserve_meta_order_subtrees_and_pagination() {
        let ctx = root().await;
        let mut writers = Vec::new();
        for n in 0..8 {
            let child = ctx.fork(ContextChildSpec::default()).unwrap();
            writers.push(tokio::spawn(async move {
                let meta = TaskMeta {
                    id: format!("task-{n}"),
                    plan_id: "parent-plan".into(),
                    ty: TaskType::Tool,
                    publisher: "publisher".into(),
                    executor: "tools".into(),
                };
                let start = child
                    .append_stack(ContextStackDraft::new(
                        meta.clone(),
                        ContextStackPhase::Started,
                    ))
                    .unwrap();
                child
                    .append_stack(ContextStackDraft::new(meta, ContextStackPhase::Succeeded))
                    .unwrap();
                let local = child.stacks(ContextStackQuery::default()).unwrap();
                assert_eq!(local.len(), 2);
                assert_eq!(local[1].parent_entry_id, Some(start));
                assert!(local[1].elapsed_ms.is_some());
            }));
        }
        for writer in writers {
            writer.await.unwrap();
        }
        assert!(ctx.stacks(ContextStackQuery::default()).unwrap().is_empty());
        let snapshot = ctx
            .stacks(ContextStackQuery {
                tree: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(snapshot.len(), 16);
        assert!(
            snapshot
                .windows(2)
                .all(|entries| entries[0].sequence < entries[1].sequence)
        );
        let filtered = ctx
            .stacks(ContextStackQuery {
                tree: true,
                task_id: Some("task-3".into()),
                plan_id: Some("parent-plan".into()),
                executor: Some("tools".into()),
                phase: Some(ContextStackPhase::Succeeded),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].task_meta.publisher, "publisher");
        let page = ctx
            .stacks(ContextStackQuery {
                tree: true,
                after_sequence: Some(snapshot[4].sequence),
                limit: Some(3),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            page.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            snapshot[5..8]
                .iter()
                .map(|e| e.sequence)
                .collect::<Vec<_>>()
        );
        ctx.append_stack(ContextStackDraft::new(
            TaskMeta::default(),
            ContextStackPhase::CleanupFinished,
        ))
        .unwrap();
        assert_eq!(snapshot.len(), 16);
    }

    #[tokio::test]
    async fn stack_capacity_failure_does_not_block_terminal_publication() {
        let ctx = root().await;
        for _ in 0..STACK_CAPACITY {
            ctx.append_stack(ContextStackDraft::new(
                TaskMeta::default(),
                ContextStackPhase::CleanupFinished,
            ))
            .unwrap();
        }
        assert!(
            ctx.append_stack(ContextStackDraft::new(
                TaskMeta::default(),
                ContextStackPhase::Failed
            ))
            .is_err()
        );
        ctx.set_state(ContextState::failed("still publishes"))
            .unwrap();
        assert_eq!(
            ctx.result::<()>().await.unwrap_err().to_string(),
            "still publishes"
        );
    }
}
#[derive(Debug)]
struct Local {
    scope: Option<Arc<ContextScope>>,
    state: watch::Sender<Arc<ContextState>>,
    children: Vec<Weak<EngineContext>>,
}
#[derive(Debug)]
pub struct EngineContext {
    engine: EngineRef,
    local: Mutex<Local>,
    stacks: Arc<Mutex<StackStore>>,
}
impl EngineContext {
    pub fn new(engine: Engine) -> Self {
        Self::with_store(
            Arc::new(engine),
            Arc::new(Mutex::new(StackStore::default())),
            None,
        )
    }
    fn with_store(
        engine: EngineRef,
        stacks: Arc<Mutex<StackStore>>,
        scope: Option<Arc<ContextScope>>,
    ) -> Self {
        let (state, _) = watch::channel(Arc::new(ContextState::Created));
        Self {
            engine,
            stacks,
            local: Mutex::new(Local {
                scope,
                state,
                children: Vec::new(),
            }),
        }
    }
    pub fn into_arc(engine: Engine) -> Arc<Self> {
        Arc::new(Self::new(engine))
    }
    fn validate(scope: &ContextScope) -> anyhow::Result<()> {
        anyhow::ensure!(
            !scope.user.user_id.trim().is_empty(),
            "user_id cannot be empty"
        );
        crate::validate_path_segment("user_id", &scope.user.user_id)?;
        anyhow::ensure!(!scope.scope_id.is_empty(), "scope_id cannot be empty");
        if let Some(session) = &scope.session {
            anyhow::ensure!(!session.session_id.is_empty(), "session_id cannot be empty");
            anyhow::ensure!(
                session.session.user_id() == scope.user.user_id,
                "session user does not match context user"
            );
        }
        Ok(())
    }
}
impl Context for EngineContext {
    fn get_engine(&self) -> EngineRef {
        self.engine.clone()
    }
    fn scope(&self) -> anyhow::Result<Arc<ContextScope>> {
        self.local
            .lock()
            .unwrap()
            .scope
            .clone()
            .ok_or_else(|| anyhow::anyhow!("context scope is not bound"))
    }
    fn bind_scope(&self, scope: ContextScope) -> anyhow::Result<()> {
        Self::validate(&scope)?;
        let mut local = self.local.lock().unwrap();
        anyhow::ensure!(local.scope.is_none(), "context scope is already bound");
        anyhow::ensure!(
            matches!(&**local.state.borrow(), ContextState::Created),
            "context must be Created when binding"
        );
        anyhow::ensure!(
            scope.parent_scope_id.is_none(),
            "root binding cannot specify a parent scope"
        );
        self.stacks
            .lock()
            .unwrap()
            .parents
            .insert(scope.scope_id.clone(), None);
        local.scope = Some(Arc::new(scope));
        Ok(())
    }
    fn fork(&self, spec: ContextChildSpec) -> anyhow::Result<Ctx> {
        let mut local = self.local.lock().unwrap();
        anyhow::ensure!(
            !local.state.borrow().is_terminal(),
            "parent context is already terminal"
        );
        let parent = local
            .scope
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("context scope is not bound"))?;
        let scope = ContextScope {
            scope_id: wd_tools::uuid::v4(),
            parent_scope_id: Some(parent.scope_id.clone()),
            user: parent.user.clone(),
            session: spec.session.or_else(|| {
                if spec.execution {
                    None
                } else {
                    parent.session.clone()
                }
            }),
            agent_id: spec.agent_id.or_else(|| {
                if spec.execution {
                    None
                } else {
                    parent.agent_id.clone()
                }
            }),
            workflow_id: spec.workflow_id.or_else(|| {
                if spec.execution {
                    None
                } else {
                    parent.workflow_id.clone()
                }
            }),
            node_id: spec.node_id.or_else(|| {
                if spec.execution {
                    None
                } else {
                    parent.node_id.clone()
                }
            }),
            turn_id: spec
                .turn_id
                .or_else(|| if spec.execution { None } else { parent.turn_id }),
        };
        Self::validate(&scope)?;
        self.stacks
            .lock()
            .unwrap()
            .parents
            .insert(scope.scope_id.clone(), scope.parent_scope_id.clone());
        let child = Arc::new(Self::with_store(
            self.engine.clone(),
            self.stacks.clone(),
            Some(Arc::new(scope)),
        ));
        local.children.retain(|child| child.strong_count() > 0);
        local.children.push(Arc::downgrade(&child));
        Ok(Ctx::new(child))
    }
    fn state(&self) -> ContextStateView {
        ContextStateView::new(self.local.lock().unwrap().state.subscribe())
    }
    fn set_state(&self, state: ContextState) -> anyhow::Result<ContextStateUpdate> {
        let children = {
            let mut local = self.local.lock().unwrap();
            let current = local.state.borrow().clone();
            anyhow::ensure!(!current.is_terminal(), "context is already terminal");
            if matches!(
                (&*current, &state),
                (ContextState::Created, ContextState::Created)
                    | (ContextState::Running, ContextState::Running)
            ) {
                return Ok(ContextStateUpdate::Unchanged);
            }
            anyhow::ensure!(
                matches!(
                    (&*current, &state),
                    (
                        ContextState::Created,
                        ContextState::Running
                            | ContextState::Failed { .. }
                            | ContextState::Cancelled { .. }
                    ) | (
                        ContextState::Running,
                        ContextState::Succeeded { .. }
                            | ContextState::Failed { .. }
                            | ContextState::Cancelled { .. }
                    )
                ),
                "invalid context state transition"
            );
            local.state.send_replace(Arc::new(state.clone()));
            local.children.retain(|child| child.strong_count() > 0);
            local
                .children
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        let reason = match state {
            ContextState::Cancelled { reason } => Some(reason),
            ContextState::Failed { error } => Some(Arc::<str>::from(error.message.clone())),
            _ => None,
        };
        if let Some(reason) = reason {
            for child in children {
                let _ = child.set_state(ContextState::Cancelled {
                    reason: reason.clone(),
                });
            }
        }
        Ok(ContextStateUpdate::Applied)
    }
    fn append_stack(&self, mut draft: ContextStackDraft) -> anyhow::Result<StackEntryId> {
        let scope = self.scope()?;
        let mut store = self.stacks.lock().unwrap();
        anyhow::ensure!(
            store.entries.len() < STACK_CAPACITY,
            "context stack capacity exceeded"
        );
        if let Some(parent) = draft.parent_entry_id {
            anyhow::ensure!(
                store.entries.iter().any(|entry| entry.entry_id == parent),
                "stack parent entry does not exist"
            );
        }
        store.sequence += 1;
        let sequence = store.sequence;
        let key = (scope.scope_id.clone(), draft.task_meta.id.clone());
        if draft.phase == ContextStackPhase::Started {
            store
                .starts
                .insert(key, (sequence, std::time::Instant::now()));
        } else if draft.phase == ContextStackPhase::Dispatched {
            store
                .starts
                .entry(key)
                .or_insert((sequence, std::time::Instant::now()));
        } else if let Some((parent, started)) = store.starts.remove(&key) {
            draft.parent_entry_id.get_or_insert(parent);
            draft
                .elapsed_ms
                .get_or_insert(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        }
        store.entries.push(ContextStack {
            entry_id: sequence,
            sequence,
            parent_entry_id: draft.parent_entry_id,
            scope_id: scope.scope_id.clone(),
            task_meta: draft.task_meta,
            phase: draft.phase,
            timestamp: chrono::Utc::now().to_rfc3339(),
            elapsed_ms: draft.elapsed_ms,
            message: draft.message.map(|s| s.chars().take(1024).collect()),
        });
        Ok(sequence)
    }
    fn stacks(&self, query: ContextStackQuery) -> anyhow::Result<Vec<ContextStack>> {
        let scope = self.scope()?;
        let store = self.stacks.lock().unwrap();
        Ok(store
            .entries
            .iter()
            .filter(|entry| {
                let mut id = Some(entry.scope_id.as_str());
                let mut belongs = false;
                while let Some(current) = id {
                    if current == scope.scope_id {
                        belongs = true;
                        break;
                    }
                    if !query.tree {
                        break;
                    }
                    id = store
                        .parents
                        .get(current)
                        .and_then(|parent| parent.as_deref());
                }
                belongs
                    && query
                        .task_id
                        .as_ref()
                        .is_none_or(|v| *v == entry.task_meta.id)
                    && query
                        .plan_id
                        .as_ref()
                        .is_none_or(|v| *v == entry.task_meta.plan_id)
                    && query
                        .executor
                        .as_ref()
                        .is_none_or(|v| *v == entry.task_meta.executor)
                    && query.phase.as_ref().is_none_or(|v| *v == entry.phase)
                    && query.after_sequence.is_none_or(|v| entry.sequence > v)
            })
            .take(query.limit.unwrap_or(256))
            .cloned()
            .collect())
    }
}
