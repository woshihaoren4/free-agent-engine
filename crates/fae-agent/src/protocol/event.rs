use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::payload::*;
use super::plugin::*;
use crate::SessionConfig;

// One registration fixes the event, payload schema and response schema together.
macro_rules! requests {
    ($($variant:ident => $suffix:literal, $request:ty, $response:ty;)+) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum RequestEvent {
            $($variant,)+
        }

        #[derive(Debug, Clone, Serialize)]
        #[serde(untagged)]
        pub enum RequestPayload {
            $($variant($request),)+
        }

        #[derive(Debug, Clone, Serialize)]
        #[serde(untagged)]
        pub enum ResponsePayload {
            $($variant($response),)+
        }

        impl RequestEvent {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => concat!("/request/", $suffix),)+ }
            }
            pub fn requires_session(self) -> bool {
                self.as_str().starts_with("/request/session/") && self != Self::SessionCreate
            }
            pub fn is_plugin(self) -> bool {
                self.as_str().starts_with("/request/plugin/")
            }
            pub fn is_session(self) -> bool {
                self.as_str().starts_with("/request/session/")
            }
        }

        impl RequestPayload {
            pub fn event(&self) -> RequestEvent {
                match self { $(Self::$variant(_) => RequestEvent::$variant,)+ }
            }
            pub(crate) fn decode(event: RequestEvent, value: Value) -> serde_json::Result<Self> {
                match event { $(RequestEvent::$variant => serde_json::from_value(value).map(Self::$variant),)+ }
            }
        }

        impl ResponsePayload {
            pub fn event(&self) -> RequestEvent {
                match self { $(Self::$variant(_) => RequestEvent::$variant,)+ }
            }
            pub(crate) fn decode(event: RequestEvent, value: Value) -> serde_json::Result<Self> {
                match event { $(RequestEvent::$variant => serde_json::from_value(value).map(Self::$variant),)+ }
            }
        }
    };
}

requests! {
    EngineHello => "engine/hello", HelloRequest, HelloResult;
    EngineStatus => "engine/status", Empty, EngineStatus;
    EngineInitialize => "engine/initialize", InitializeRequest, InitializeResult;
    EngineShutdown => "engine/shutdown", Empty, ShutdownResult;
    EnginePluginShow => "engine/plugin/show", PluginShowRequest, PluginShowResult;
    EnginePluginInstall => "engine/plugin/install", PluginInstallRequest, PluginMutationResult;
    EnginePluginUninstall => "engine/plugin/uninstall", PluginUninstallRequest, PluginMutationResult;
    PlanCall => "plan/call", Value, Value;
    RuntimeExec => "runtime/exec", Value, Value;
    RuntimeSelect => "runtime/select", Value, Value;
    StreamNext => "stream/next", BoundaryStreamRequest, Value;
    StreamClose => "stream/close", BoundaryStreamRequest, Empty;
    SessionCreate => "session/create", SessionCreateRequest, SessionSummary;
    SessionGet => "session/get", Empty, SessionSummary;
    SessionConfigUpdate => "session/config/update", ConfigUpdateRequest, SessionConfig;
    SessionConversationList => "session/conversation/list", ConversationListRequest, ConversationPage;
    SessionConversationClear => "session/conversation/clear", Empty, ConversationClearResult;
    SessionExecutionStart => "session/execution/start", ExecutionStartRequest, ExecutionAccepted;
    SessionExecutionSteer => "session/execution/steer", ExecutionSteerRequest, InputAccepted;
    SessionExecutionStop => "session/execution/stop", ExecutionStopRequest, ExecutionStopResult;
    SessionExecutionGet => "session/execution/get", ExecutionGetRequest, ExecutionGetResult;
    SessionSubscribe => "session/subscribe", SubscribeRequest, SubscribeResult;
    SessionUnsubscribe => "session/unsubscribe", UnsubscribeRequest, UnsubscribeResult;
    SessionArchive => "session/archive", Empty, SessionSummary;
    PluginWorkerHello => "plugin/worker/hello", PluginHello, Empty;
    PluginInspect => "plugin/inspect", PluginInspect, crate::plugin::PluginDeclaration;
    PluginInvoke => "plugin/invoke", PluginInvoke, Value;
    PluginNext => "plugin/next", PluginNext, Value;
    PluginContextGet => "plugin/context/get", PluginAccess, SessionSummary;
    PluginContextExport => "plugin/context/export", PluginAccess, crate::ContextData;
    PluginContextStacks => "plugin/context/stacks", PluginStacks, Vec<crate::ContextStack>;
    PluginContextFork => "plugin/context/fork", PluginFork, PluginContextRef;
    PluginContextCancel => "plugin/context/cancel", PluginCancel, Empty;
    PluginContextCompleteQuery => "plugin/context/complete_query", PluginQueryCompletion, Empty;
    PluginOutputSend => "plugin/output/send", PluginOutput, PluginOutputReceipt;
    PluginStreamNext => "plugin/stream/next", PluginStream, Value;
    PluginStreamClose => "plugin/stream/close", PluginStream, Empty;
    PluginCallRelease => "plugin/call/release", PluginRelease, Empty;
    PluginHostEngine => "plugin/host/get_engine", PluginAccess, PluginEngineRef;
    PluginHostSession => "plugin/host/get_session", PluginAccess, Option<PluginContextRef>;
}

macro_rules! events {
    ($($variant:ident => $suffix:literal, $payload:ty;)+) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum NotificationEvent {
            $($variant,)+
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
        #[serde(untagged)]
        pub enum EventPayload {
            $($variant($payload),)+
        }

        impl NotificationEvent {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => concat!("/event/", $suffix),)+ }
            }
        }

        impl EventPayload {
            pub fn event(&self) -> NotificationEvent {
                match self { $(Self::$variant(_) => NotificationEvent::$variant,)+ }
            }
            pub(crate) fn decode(event: NotificationEvent, value: Value) -> serde_json::Result<Self> {
                match event { $(NotificationEvent::$variant => serde_json::from_value(value).map(Self::$variant),)+ }
            }
        }
    };
}

events! {
    ConfigUpdated => "session/config/updated", ConfigUpdated;
    ConversationAppended => "session/conversation/appended", ConversationAppended;
    ExecutionStateChanged => "session/execution/state_changed", ExecutionStateChanged;
    InputApplied => "session/execution/input_applied", InputApplied;
    InputNotApplied => "session/execution/input_not_applied", InputNotApplied;
    InvocationChanged => "session/execution/invocation_changed", InvocationChanged;
    OutputDelta => "session/execution/output_delta", OutputDelta;
    ExecutionFinished => "session/execution/finished", ExecutionFinished;
}

macro_rules! event_serde {
    ($($ty:ty),+ $(,)?) => {$(
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::ALL.iter().copied().find(|event| event.as_str() == value)
                    .ok_or_else(|| serde::de::Error::custom(format!("unregistered protocol event `{value}`")))
            }
        }
    )+};
}

event_serde!(RequestEvent, NotificationEvent);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageEvent {
    Request(RequestEvent),
    Event(NotificationEvent),
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MessagePayload {
    Request(RequestPayload),
    Event(EventPayload),
}

impl MessagePayload {
    pub fn event(&self) -> MessageEvent {
        match self {
            Self::Request(payload) => MessageEvent::Request(payload.event()),
            Self::Event(payload) => MessageEvent::Event(payload.event()),
        }
    }
}
