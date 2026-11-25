use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::payload::*;
use super::plugin::*;
use crate::SessionConfig;

// One registration fixes the path, the payload schema and the response schema together.
macro_rules! requests {
    ($($variant:ident => $suffix:literal, $request:ty, $response:ty;)+) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum RequestPath {
            $($variant,)+
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum ResponsePath {
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

        impl RequestPath {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => concat!("/request/", $suffix),)+ }
            }
            pub fn response(self) -> ResponsePath {
                match self { $(Self::$variant => ResponsePath::$variant,)+ }
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

        impl ResponsePath {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => concat!("/response/", $suffix),)+ }
            }
            pub fn request(self) -> RequestPath {
                match self { $(Self::$variant => RequestPath::$variant,)+ }
            }
        }

        impl RequestPayload {
            pub fn path(&self) -> RequestPath {
                match self { $(Self::$variant(_) => RequestPath::$variant,)+ }
            }
            pub(crate) fn decode(path: RequestPath, value: Value) -> serde_json::Result<Self> {
                match path { $(RequestPath::$variant => serde_json::from_value(value).map(Self::$variant),)+ }
            }
        }

        impl ResponsePayload {
            pub fn path(&self) -> ResponsePath {
                match self { $(Self::$variant(_) => ResponsePath::$variant,)+ }
            }
            pub(crate) fn decode(path: ResponsePath, value: Value) -> serde_json::Result<Self> {
                match path { $(ResponsePath::$variant => serde_json::from_value(value).map(Self::$variant),)+ }
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
        pub enum EventPath {
            $($variant,)+
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
        #[serde(untagged)]
        pub enum EventPayload {
            $($variant($payload),)+
        }

        impl EventPath {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => concat!("/event/", $suffix),)+ }
            }
        }

        impl EventPayload {
            pub fn path(&self) -> EventPath {
                match self { $(Self::$variant(_) => EventPath::$variant,)+ }
            }
            pub(crate) fn decode(path: EventPath, value: Value) -> serde_json::Result<Self> {
                match path { $(EventPath::$variant => serde_json::from_value(value).map(Self::$variant),)+ }
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

macro_rules! path_serde {
    ($($ty:ty),+ $(,)?) => {$(
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::ALL.iter().copied().find(|path| path.as_str() == value)
                    .ok_or_else(|| serde::de::Error::custom(format!("unregistered protocol path `{value}`")))
            }
        }
    )+};
}

path_serde!(RequestPath, ResponsePath, EventPath);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessagePath {
    Request(RequestPath),
    Event(EventPath),
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MessagePayload {
    Request(RequestPayload),
    Event(EventPayload),
}

impl MessagePayload {
    pub fn path(&self) -> MessagePath {
        match self {
            Self::Request(payload) => MessagePath::Request(payload.path()),
            Self::Event(payload) => MessagePath::Event(payload.path()),
        }
    }
}
