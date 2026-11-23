"""Integer response codes are independent of structured execution failures."""
from enum import IntEnum
from typing import Any


class ResponseCode(IntEnum):
    SUCCESS = 0
    BAD_REQUEST = 400
    FORBIDDEN = 403
    NOT_FOUND = 404
    CONFLICT = 409
    INTERNAL_ERROR = 500
    UNAVAILABLE = 503
    STORAGE_ERROR = 507


class PluginHostError(Exception):
    code = ResponseCode.INTERNAL_ERROR

    def __init__(self, message: str, **details: Any):
        super().__init__(message)
        self.message, self.details = message, details


class ContextUnavailable(PluginHostError):
    code = ResponseCode.NOT_FOUND


class ContextFinished(PluginHostError):
    code = ResponseCode.CONFLICT


class HandleExpired(PluginHostError):
    code = ResponseCode.NOT_FOUND


class UnsupportedValueType(PluginHostError):
    code = ResponseCode.BAD_REQUEST


class SessionBusy(PluginHostError):
    code = ResponseCode.CONFLICT


class SessionUnavailable(PluginHostError):
    code = ResponseCode.NOT_FOUND


class InvalidMessage(PluginHostError):
    code = ResponseCode.BAD_REQUEST


class HostCallTimeout(PluginHostError):
    code = ResponseCode.UNAVAILABLE


class HookCancelled(PluginHostError):
    code = ResponseCode.CONFLICT


class InvalidHookNext(PluginHostError):
    code = ResponseCode.CONFLICT


class MissingChainHandler(PluginHostError):
    code = ResponseCode.BAD_REQUEST


def from_wire(error: dict[str, Any]) -> PluginHostError:
    code = ResponseCode(error["code"])
    cls = {ResponseCode.NOT_FOUND: HandleExpired, ResponseCode.CONFLICT: ContextFinished,
           ResponseCode.BAD_REQUEST: InvalidMessage, ResponseCode.UNAVAILABLE: HostCallTimeout}.get(code, PluginHostError)
    result = cls(error["msg"])
    result.code = code
    return result
