use crate::Engine;
use fae_agent::{AnyType, Context, EngineRef};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

enum Completion {
    Pending,
    Ready(Result<AnyType, String>),
    Aborted,
    Consumed { aborted: bool },
}

pub struct EngineContext {
    engine: EngineRef,
    stacks: Mutex<HashMap<String, Vec<String>>>,
    result: Mutex<Completion>,
    completion: Notify,
}

impl Debug for EngineContext {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngineContext")
            .field("engine", &self.engine)
            .field("stacks", &self.stacks)
            .field("completed", &self.is_completed())
            .field("aborted", &self.is_aborted())
            .finish_non_exhaustive()
    }
}

impl EngineContext {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine: Arc::new(engine),
            stacks: Mutex::new(HashMap::new()),
            result: Mutex::new(Completion::Pending),
            completion: Notify::new(),
        }
    }

    pub fn into_arc(engine: Engine) -> Arc<Self> {
        Arc::new(Self::new(engine))
    }

    pub fn stacks(&self) -> HashMap<String, Vec<String>> {
        self.stacks.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Context for EngineContext {
    fn append_stack(&self, key: &str, value: String) {
        self.stacks
            .lock()
            .unwrap()
            .entry(key.to_string())
            .or_default()
            .push(value);
    }

    fn stacks(&self) -> HashMap<String, Vec<String>> {
        self.stacks.lock().unwrap().clone()
    }

    fn get_engine(&self) -> EngineRef {
        self.engine.clone()
    }

    fn abort(&self) {
        {
            let mut completion = self.result.lock().unwrap();
            if !matches!(&*completion, Completion::Pending) {
                return;
            }
            *completion = Completion::Aborted;
        }

        self.completion.notify_waiters();
    }

    fn is_aborted(&self) -> bool {
        matches!(
            &*self.result.lock().unwrap(),
            Completion::Aborted | Completion::Consumed { aborted: true }
        )
    }

    fn over(&self, value: AnyType) {
        self.finish(Ok(value));
    }

    fn error(&self, error: String) {
        self.finish(Err(error));
    }

    fn is_completed(&self) -> bool {
        !matches!(&*self.result.lock().unwrap(), Completion::Pending)
    }

    async fn wait(&self) -> anyhow::Result<AnyType> {
        loop {
            let notified = self.completion.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let result = {
                let mut result = self.result.lock().unwrap();
                match &*result {
                    Completion::Pending => None,
                    Completion::Consumed { .. } => {
                        anyhow::bail!("context result has already been consumed")
                    }
                    Completion::Ready(_) => {
                        let Completion::Ready(value) = std::mem::replace(
                            &mut *result,
                            Completion::Consumed { aborted: false },
                        ) else {
                            unreachable!()
                        };
                        Some(value)
                    }
                    Completion::Aborted => {
                        *result = Completion::Consumed { aborted: true };
                        return Err(fae_agent::Error::ContextAborted.into());
                    }
                }
            };
            if let Some(result) = result {
                return result.map_err(anyhow::Error::msg);
            }

            notified.await;
        }
    }
}

impl EngineContext {
    fn finish(&self, result: Result<AnyType, String>) {
        {
            let mut completion = self.result.lock().unwrap();
            if !matches!(&*completion, Completion::Pending) {
                return;
            }
            *completion = Completion::Ready(result);
        }

        self.completion.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use crate::EngineBuilder;

    #[tokio::test]
    async fn abort_completes_context_with_abort_error() {
        let engine = EngineBuilder::new().build().await;
        let ctx = engine.ctx();

        ctx.abort();

        assert!(ctx.is_aborted());
        assert!(ctx.is_completed());
        assert_eq!(
            ctx.result::<()>().await.unwrap_err().to_string(),
            "context has been aborted"
        );
        assert!(ctx.is_aborted());
    }
}
