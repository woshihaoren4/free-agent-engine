//! Host-owned timer contracts. Model arguments never carry execution identity.
use crate::{Ctx, SingleAgentSource};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerOwner {
    pub agent_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerExecutionBinding {
    pub owner: TimerOwner,
    pub source: SingleAgentSource,
    pub workspace: PathBuf,
}

pub trait TimerExecutionBindingResolver: std::fmt::Debug + Send + Sync {
    fn resolve(&self, ctx: &Ctx) -> anyhow::Result<TimerExecutionBinding>;
}

#[derive(Debug, Default)]
pub struct ScopedTimerBindingResolver;
impl TimerExecutionBindingResolver for ScopedTimerBindingResolver {
    fn resolve(&self, ctx: &Ctx) -> anyhow::Result<TimerExecutionBinding> {
        let scope = ctx.scope()?;
        let binding = scope
            .timer_execution
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("execution_binding_unavailable"))?;
        anyhow::ensure!(
            scope.agent_id.as_deref() == Some(&binding.owner.agent_id)
                && scope.user.user_id == binding.owner.user_id,
            "execution_binding_unavailable: ownership mismatch"
        );
        Ok((**binding).clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TimerSchedule {
    Once {
        cron: String,
        #[serde(default = "utc")]
        timezone: String,
    },
    Repeat {
        cron: String,
        #[serde(default = "utc")]
        timezone: String,
        max_fires: u64,
    },
    Loop {
        cron: String,
        #[serde(default = "utc")]
        timezone: String,
    },
}
fn utc() -> String {
    "UTC".into()
}
impl TimerSchedule {
    pub fn cron(&self) -> &str {
        match self {
            Self::Once { cron, .. } | Self::Repeat { cron, .. } | Self::Loop { cron, .. } => cron,
        }
    }
    pub fn timezone(&self) -> &str {
        match self {
            Self::Once { timezone, .. }
            | Self::Repeat { timezone, .. }
            | Self::Loop { timezone, .. } => timezone,
        }
    }
    pub fn set_cron(&mut self, value: String) {
        match self {
            Self::Once { cron, .. } | Self::Repeat { cron, .. } | Self::Loop { cron, .. } => {
                *cron = value
            }
        }
    }
    pub fn limit(&self) -> Option<u64> {
        match self {
            Self::Once { .. } => Some(1),
            Self::Repeat { max_fires, .. } => Some(*max_fires),
            Self::Loop { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerMisfirePolicy {
    #[default]
    Coalesce,
    Skip,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerState {
    Active,
    Paused,
    Completed,
    Deleted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerOccurrenceStatus {
    Pending,
    Dispatching,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Suppressed,
}
impl TimerOccurrenceStatus {
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Dispatching | Self::Running)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerDefinition {
    pub id: String,
    pub revision: u64,
    pub owner: TimerOwner,
    pub created_session_id: String,
    pub execution_binding: TimerExecutionBinding,
    pub name: String,
    pub input: String,
    pub schedule: TimerSchedule,
    pub misfire_policy: TimerMisfirePolicy,
    pub state: TimerState,
    pub completion_reason: Option<String>,
    pub accepted_count: u64,
    pub next_fire_at: Option<String>,
    pub queued_at: Option<String>,
    pub last_scheduled_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_occurrence_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerOccurrence {
    pub occurrence_id: String,
    pub timer_id: String,
    pub definition_revision: u64,
    pub owner: TimerOwner,
    pub execution_binding: TimerExecutionBinding,
    pub input: String,
    pub session_id: String,
    pub scheduled_at: String,
    pub emitted_at: String,
    pub status: TimerOccurrenceStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub round_id: Option<String>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerFired {
    pub owner: TimerOwner,
    pub timer_id: String,
    pub occurrence_id: String,
    pub session_id: String,
    pub definition_revision: u64,
    pub scheduled_at: String,
    pub emitted_at: String,
}
impl From<&TimerOccurrence> for TimerFired {
    fn from(o: &TimerOccurrence) -> Self {
        Self {
            owner: o.owner.clone(),
            timer_id: o.timer_id.clone(),
            occurrence_id: o.occurrence_id.clone(),
            session_id: o.session_id.clone(),
            definition_revision: o.definition_revision,
            scheduled_at: o.scheduled_at.clone(),
            emitted_at: o.emitted_at.clone(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum TimerRequest {
    Create {
        idempotency_key: String,
        name: String,
        input: String,
        schedule: TimerSchedule,
        #[serde(default)]
        misfire_policy: TimerMisfirePolicy,
    },
    Query {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        state: Option<TimerState>,
        #[serde(default)]
        include_deleted: bool,
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default)]
        include_occurrences: bool,
        #[serde(default = "page_limit")]
        limit: usize,
    },
    Update {
        id: String,
        expected_revision: u64,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        input: Option<String>,
        #[serde(default)]
        schedule: Option<TimerSchedule>,
        #[serde(default)]
        misfire_policy: Option<TimerMisfirePolicy>,
        #[serde(default)]
        enabled: Option<bool>,
    },
    Delete {
        id: String,
        #[serde(default)]
        expected_revision: Option<u64>,
    },
}
fn page_limit() -> usize {
    20
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimerResponse {
    pub timers: Vec<TimerDefinition>,
    pub occurrences: Vec<TimerOccurrence>,
    pub next_local: Vec<String>,
    pub preview: Vec<String>,
    pub next_cursor: Option<String>,
}
pub type TimerQuery = ();
pub type TimerList = TimerResponse;
