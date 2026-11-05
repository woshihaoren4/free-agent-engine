#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode { Agent, Workflow }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunState { Idle, Running, Completed, Failed }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageKind {
    User, Assistant, Reasoning, Tool, Workflow, Notice, System, LogOutput, LogError, Error,
}
impl MessageKind {
    pub(super) fn folds_by_default(self) -> bool {
        !matches!(self, Self::User | Self::Assistant | Self::Error)
    }
}

#[derive(Debug)]
pub(super) struct Message {
    pub(super) kind: MessageKind,
    pub(super) title: String,
    pub(super) content: String,
    pub(super) stream_id: Option<String>,
    pub(super) expanded: bool,
    pub(super) render_markdown: bool,
}
#[derive(Debug)]
pub(super) struct BufferedMessage {
    pub(super) kind: MessageKind,
    pub(super) title: String,
    pub(super) content: String,
}
impl Message {
    pub(super) fn new(kind: MessageKind, title: impl Into<String>, content: impl Into<String>, stream_id: Option<String>) -> Self {
        let content = content.into();
        Self { kind, title: title.into(), expanded: !kind.folds_by_default() || content.is_empty(), content, stream_id, render_markdown: false }
    }
    pub(super) fn is_collapsible(&self) -> bool {
        self.kind.folds_by_default() && !self.content.is_empty()
    }
    pub(super) fn shows_content(&self) -> bool { !self.is_collapsible() || self.expanded }
}
