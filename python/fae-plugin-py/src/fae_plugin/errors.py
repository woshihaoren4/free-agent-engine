"""Stable errors returned by the host and the plugin dispatcher."""
from typing import Any


class PluginHostError(Exception):
    code = "plugin_error"

    def __init__(self, message: str, **details: Any) -> None:
        super().__init__(message)
        self.message = message
        self.details = details


class ContextUnavailable(PluginHostError):
    code = "context_unavailable"


class ContextFinished(PluginHostError):
    code = "context_finished"


class InvalidStateTransition(PluginHostError):
    code = "invalid_state_transition"


class ScopeAlreadyBound(PluginHostError):
    code = "scope_already_bound"


class InvalidScope(PluginHostError):
    code = "invalid_scope"


class InvalidHandle(PluginHostError):
    code = "invalid_handle"


class HandleExpired(PluginHostError):
    code = "handle_expired"


class StateViewClosed(PluginHostError):
    code = "state_view_closed"


class UnsupportedOutputType(PluginHostError):
    code = "unsupported_output_type"


class UnsupportedValueType(PluginHostError):
    code = "unsupported_value_type"


class StackCapacityExceeded(PluginHostError):
    code = "stack_capacity_exceeded"


class InvalidStackEntry(PluginHostError):
    code = "invalid_stack_entry"


class SessionBusy(PluginHostError):
    code = "session_busy"


class SessionUnavailable(PluginHostError):
    code = "session_unavailable"


class InvalidMessage(PluginHostError):
    code = "invalid_message"


class HostCallTimeout(PluginHostError):
    code = "host_call_timeout"


class HookCancelled(PluginHostError):
    code = "hook_cancelled"


class InvalidHookNext(PluginHostError):
    code = "invalid_hook_next"


class MissingChainHandler(PluginHostError):
    code = "missing_chain_handler"


def from_wire(error: dict[str, Any]) -> PluginHostError:
    cls = next((c for c in PluginHostError.__subclasses__()
                if c.code == error.get("code")), PluginHostError)
    result = cls(error.get("message", "Plugin host error"),
                 **{k: v for k, v in error.items() if k not in ("code", "message")})
    result.code = error.get("code", result.code)
    return result
