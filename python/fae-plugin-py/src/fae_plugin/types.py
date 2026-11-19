"""Immutable snapshots and native request/response wrappers for API 0.1.0."""
from __future__ import annotations

import json
import math
from dataclasses import dataclass, fields, is_dataclass
from enum import Enum
from typing import Any, AsyncIterator, Literal, TYPE_CHECKING, TypeAlias

if TYPE_CHECKING:
    from .session import Session

JsonValue: TypeAlias = None | bool | int | float | str | list["JsonValue"] | dict[str, "JsonValue"]
StackEntryId: TypeAlias = int
API_VERSION = "0.1.0"


def json_value(value: Any) -> JsonValue:
    if value is None or isinstance(value, (bool, str)):
        return value
    if isinstance(value, int):
        if not -(1 << 63) <= value < (1 << 64):
            raise ValueError("JSON integer is outside the host range")
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValueError("Non-finite JSON number")
        return value
    if isinstance(value, list):
        return [json_value(v) for v in value]
    if isinstance(value, dict) and all(isinstance(k, str) for k in value):
        return {k: json_value(v) for k, v in value.items()}
    raise TypeError(f"Not a JSON value: {type(value).__name__}")


def u64(value: int | None) -> None:
    if value is not None and (type(value) is not int or not 0 <= value < (1 << 64)):
        raise ValueError("Expected an unsigned 64-bit integer")


class RegistrationMode(str, Enum):
    CHAIN = "chain"
    DIRECT = "direct"


class ContextStateKind(str, Enum):
    CREATED = "created"
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"


class ContextStateUpdate(str, Enum):
    APPLIED = "applied"
    UNCHANGED = "unchanged"


@dataclass(frozen=True)
class ContextFailure:
    message: str

    def __post_init__(self) -> None:
        if not isinstance(self.message, str):
            raise TypeError("failure message must be a string")


class ContextOutput:
    __slots__ = ("_value", "_host", "_handle")

    def __init__(self, value: JsonValue, *, _host: Any = None, _handle: str | None = None):
        self._value = json_value(value)
        self._host = _host
        self._handle = _handle

    @classmethod
    def from_json(cls, value: JsonValue) -> ContextOutput:
        return cls(value)

    async def json_snapshot(self) -> JsonValue:
        if self._handle is not None:
            return await self._host.call("output.json_snapshot", {"handle": self._handle})
        return json_value(self._value)


@dataclass(frozen=True)
class ContextState:
    kind: ContextStateKind
    output: ContextOutput | None = None
    error: ContextFailure | None = None
    reason: str | None = None

    def __post_init__(self) -> None:
        if not isinstance(self.kind, ContextStateKind):
            raise TypeError("kind must be ContextStateKind")
        actual = (self.output is not None, self.error is not None, self.reason is not None)
        expected = {
            ContextStateKind.SUCCEEDED: (True, False, False),
            ContextStateKind.FAILED: (False, True, False),
            ContextStateKind.CANCELLED: (False, False, True),
        }.get(self.kind, (False, False, False))
        if actual != expected:
            raise ValueError("State fields do not match its kind")
        if self.output is not None and not isinstance(self.output, ContextOutput):
            raise TypeError("output must be ContextOutput")
        if self.error is not None and not isinstance(self.error, ContextFailure):
            raise TypeError("error must be ContextFailure")
        if self.reason is not None and not isinstance(self.reason, str):
            raise TypeError("reason must be str")

    @classmethod
    def created(cls) -> ContextState:
        return cls(ContextStateKind.CREATED)

    @classmethod
    def running(cls) -> ContextState:
        return cls(ContextStateKind.RUNNING)

    @classmethod
    def succeeded(cls, value: JsonValue) -> ContextState:
        return cls(ContextStateKind.SUCCEEDED, output=ContextOutput.from_json(value))

    @classmethod
    def failed(cls, message: str) -> ContextState:
        return cls(ContextStateKind.FAILED, error=ContextFailure(message))

    @classmethod
    def cancelled(cls, reason: str) -> ContextState:
        return cls(ContextStateKind.CANCELLED, reason=reason)

    def is_terminal(self) -> bool:
        return self.kind in (ContextStateKind.SUCCEEDED, ContextStateKind.FAILED, ContextStateKind.CANCELLED)

    def is_cancelled(self) -> bool:
        return self.kind == ContextStateKind.CANCELLED


class ContextStackPhase(str, Enum):
    DISPATCHED = "dispatched"
    STARTED = "started"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"
    CLEANUP_FINISHED = "cleanup_finished"


class TaskTypeKind(str, Enum):
    TOOL = "tool"
    SKILL = "skill"
    MCP = "mcp"
    PLAN = "plan"
    WORKFLOW = "workflow"
    MODEL = "model"
    SESSION = "session"
    MEMORY = "memory"
    ANY = "any"


@dataclass(frozen=True)
class TaskType:
    kind: TaskTypeKind = TaskTypeKind.TOOL
    value: str | None = None

    def __post_init__(self) -> None:
        if not isinstance(self.kind, TaskTypeKind):
            raise TypeError("kind must be TaskTypeKind")
        if (self.kind == TaskTypeKind.ANY) != (isinstance(self.value, str) and bool(self.value)):
            if self.kind == TaskTypeKind.ANY or self.value is not None:
                raise ValueError("Only ANY task type requires a value")


@dataclass(frozen=True)
class TaskMeta:
    id: str
    plan_id: str
    ty: TaskType
    publisher: str
    executor: str

    def __post_init__(self) -> None:
        if not isinstance(self.ty, TaskType) or not all(
            isinstance(v, str) for v in (self.id, self.plan_id, self.publisher, self.executor)
        ):
            raise TypeError("Invalid TaskMeta fields")


@dataclass(frozen=True)
class ContextStackDraft:
    task_meta: TaskMeta
    phase: ContextStackPhase
    parent_entry_id: StackEntryId | None = None
    elapsed_ms: int | None = None
    message: str | None = None

    def __post_init__(self) -> None:
        u64(self.parent_entry_id)
        u64(self.elapsed_ms)
        if not isinstance(self.phase, ContextStackPhase):
            raise TypeError("phase must be ContextStackPhase")
        if not isinstance(self.task_meta, TaskMeta) or (
            self.message is not None and not isinstance(self.message, str)
        ):
            raise TypeError("Invalid stack draft fields")


@dataclass(frozen=True)
class ContextStack:
    entry_id: StackEntryId
    parent_entry_id: StackEntryId | None
    sequence: int
    scope_id: str
    task_meta: TaskMeta
    phase: ContextStackPhase
    timestamp: str
    elapsed_ms: int | None
    message: str | None


@dataclass(frozen=True)
class ContextStackQuery:
    tree: bool = False
    task_id: str | None = None
    plan_id: str | None = None
    executor: str | None = None
    phase: ContextStackPhase | None = None
    after_sequence: int | None = None
    limit: int | None = None

    def __post_init__(self) -> None:
        if type(self.tree) is not bool or (
            self.phase is not None and not isinstance(self.phase, ContextStackPhase)
        ):
            raise TypeError("Invalid stack query flags")
        if any(v is not None and not isinstance(v, str)
               for v in (self.task_id, self.plan_id, self.executor)):
            raise TypeError("Invalid stack query filters")
        u64(self.after_sequence)
        u64(self.limit)
        if self.limit is not None and self.limit > 256:
            raise ValueError("Stack query limit exceeds 256")


@dataclass(frozen=True)
class ContextUser:
    user_id: str


@dataclass(frozen=True)
class ContextSession:
    session_id: str
    session: Session


@dataclass(frozen=True)
class TodoPlanScope:
    plan_id: str
    agent_id: str
    user_id: str
    user_input: str
    _handle: str = ""


@dataclass(frozen=True)
class TodoContextBinding:
    kind: Literal["auto", "inherit", "clear", "bind"] = "auto"
    scope: TodoPlanScope | None = None

    def __post_init__(self) -> None:
        if self.kind not in ("auto", "inherit", "clear", "bind"):
            raise ValueError("Invalid todo binding")
        if (self.kind == "bind") != (self.scope is not None):
            raise ValueError("bind requires a todo scope")
        if self.scope is not None and not isinstance(self.scope, TodoPlanScope):
            raise TypeError("scope must be TodoPlanScope")


@dataclass(frozen=True)
class SingleAgentSource:
    kind: Literal["agent_id", "paths"]
    agent_id: str | None = None
    config: str | None = None
    prompt: str | None = None

    def __post_init__(self) -> None:
        if self.kind == "agent_id":
            if not self.agent_id or self.config is not None or self.prompt is not None:
                raise ValueError("Invalid agent_id source")
        elif self.kind == "paths":
            if self.agent_id is not None or not self.config or not self.prompt:
                raise ValueError("Invalid paths source")
        else:
            raise ValueError("Invalid source kind")


@dataclass(frozen=True)
class TimerOwner:
    agent_id: str
    user_id: str


@dataclass(frozen=True)
class TimerExecutionBinding:
    owner: TimerOwner
    source: SingleAgentSource
    workspace: str


@dataclass(frozen=True)
class ContextScope:
    scope_id: str
    parent_scope_id: str | None
    user: ContextUser
    session: ContextSession | None = None
    agent_id: str | None = None
    workflow_id: str | None = None
    node_id: str | None = None
    turn_id: int | None = None
    todo: TodoPlanScope | None = None
    workspace: str | None = None
    execution_origin: str | None = None
    timer_execution: TimerExecutionBinding | None = None

    def __post_init__(self) -> None:
        u64(self.turn_id)
        if not isinstance(self.scope_id, str) or not isinstance(self.user, ContextUser):
            raise TypeError("Invalid scope identity")
        if any(v is not None and not isinstance(v, str) for v in (
            self.parent_scope_id, self.agent_id, self.workflow_id, self.node_id,
            self.workspace, self.execution_origin,
        )):
            raise TypeError("Invalid scope fields")


@dataclass(frozen=True)
class ContextChildSpec:
    timer_source: SingleAgentSource | None = None
    timer_workspace: str | None = None
    workspace: str | None = None
    session: ContextSession | None = None
    agent_id: str | None = None
    workflow_id: str | None = None
    node_id: str | None = None
    turn_id: int | None = None
    execution: bool = False
    todo: TodoContextBinding = TodoContextBinding()

    def __post_init__(self) -> None:
        u64(self.turn_id)
        if type(self.execution) is not bool or not isinstance(self.todo, TodoContextBinding):
            raise TypeError("Invalid child execution or todo binding")
        if self.session is not None and not isinstance(self.session, ContextSession):
            raise TypeError("session must be ContextSession")
        if self.timer_source is not None and not isinstance(self.timer_source, SingleAgentSource):
            raise TypeError("timer_source must be SingleAgentSource")
        if any(v is not None and not isinstance(v, str) for v in (
            self.timer_workspace, self.workspace, self.agent_id, self.workflow_id, self.node_id,
        )):
            raise TypeError("Invalid child scope fields")


@dataclass(frozen=True)
class MessageReceipt:
    message_id: str


class NativeObject:
    """OpenAI/native JSON with attribute access; unknown fields remain lossless."""
    def __init__(self, data: dict[str, Any]):
        object.__setattr__(self, "_data", json_value(data))

    def __getattr__(self, name: str) -> Any:
        try:
            value = self._data[name]
        except KeyError:
            if name == "stream":
                return False
            raise AttributeError(name) from None
        if isinstance(value, dict):
            return NativeObject(value)
        if isinstance(value, list):
            return [NativeObject(v) if isinstance(v, dict) else v for v in value]
        return value

    def __setattr__(self, name: str, value: Any) -> None:
        self._data[name] = encode(value)

    def to_dict(self) -> dict[str, Any]:
        return json_value(self._data)  # type: ignore[return-value]


class ModelRequest(NativeObject):
    pass


class SessionMessage(NativeObject):
    pass


class UserMemory(NativeObject):
    pass


class ToolRequest(NativeObject):
    def arguments_json(self) -> JsonValue:
        return json_value(json.loads(self.arguments))


class ToolResponse:
    def __init__(self, result: str | None = None, *, _reference: str | None = None):
        self.result = result
        self._reference = _reference

    @classmethod
    def completed(cls, result: str) -> ToolResponse:
        if not isinstance(result, str):
            raise TypeError("Tool result must be a string")
        return cls(result)


class ModelResponse:
    def __init__(self, value: dict[str, Any] | AsyncIterator[dict[str, Any]], *, streaming: bool):
        self.value = value
        self.is_streaming = streaming

    @classmethod
    def completed(cls, value: dict[str, Any]) -> ModelResponse:
        if not isinstance(value, dict):
            raise TypeError("Model completion must be an OpenAI response object")
        return cls(json_value(value), streaming=False)  # type: ignore[arg-type]

    @classmethod
    def streaming(cls, chunks: AsyncIterator[dict[str, Any]]) -> ModelResponse:
        if not hasattr(chunks, "__aiter__"):
            raise TypeError("Model stream must be an async iterator")
        return cls(chunks, streaming=True)

    def __aiter__(self) -> AsyncIterator[dict[str, Any]]:
        if not self.is_streaming:
            raise TypeError("Completed response is not a stream")
        return self.value.__aiter__()  # type: ignore[union-attr]


def encode(value: Any) -> Any:
    if isinstance(value, NativeObject):
        return value.to_dict()
    if isinstance(value, Enum):
        return value.value
    if isinstance(value, ContextOutput):
        if value._handle:
            return {"handle": value._handle}
        return {"json": json_value(value._value)}
    if hasattr(value, "_wire_reference"):
        return value._wire_reference()
    if is_dataclass(value):
        result = {f.name: encode(getattr(value, f.name)) for f in fields(value)
                  if getattr(value, f.name) is not None and not f.name.startswith("_")}
        if isinstance(value, TodoPlanScope):
            result["handle"] = value._handle
        return result
    if isinstance(value, (list, tuple)):
        return [encode(v) for v in value]
    if isinstance(value, dict):
        return {k: encode(v) for k, v in value.items()}
    return json_value(value)
