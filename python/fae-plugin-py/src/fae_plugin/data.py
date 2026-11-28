"""Serializable public facts. Decoding never constructs a capability proxy."""
from __future__ import annotations
from dataclasses import dataclass
from enum import Enum
from typing import Any, Literal, get_args, get_origin, get_type_hints, Union
from types import UnionType
from .tasks import TaskMetadata


def _decode(ty: Any, value: Any) -> Any:
    origin, args = get_origin(ty), get_args(ty)
    if ty is Any:
        return value
    if ty is TaskMetadata:
        return TaskMetadata.from_dict(value)
    if origin in (Union, UnionType):
        for alternative in args:
            try:
                return _decode(alternative, value)
            except (TypeError, ValueError, KeyError):
                pass
        raise ValueError("Value does not match registered variants")
    if origin is Literal:
        if value not in args:
            raise ValueError("Invalid JSON tag")
        return value
    if origin is list:
        if not isinstance(value, list):
            raise TypeError("Expected list")
        return [_decode(args[0], item) for item in value]
    if isinstance(ty, type) and issubclass(ty, DTO):
        return ty.from_dict(value)
    if isinstance(ty, type) and issubclass(ty, Enum):
        return ty(value)
    if ty is type(None):
        if value is not None:
            raise TypeError("Expected null")
    elif ty in (str, int, bool):
        if type(value) is not ty:
            raise TypeError(f"Expected {ty.__name__}")
    elif ty is dict and not isinstance(value, dict):
        raise TypeError("Expected JSON object")
    return value


class DTO:
    @classmethod
    def from_dict(cls, value: dict) -> Any:
        hints = get_type_hints(cls)
        if not isinstance(value, dict) or set(value) != set(hints):
            raise ValueError(f"Invalid {cls.__name__} fields")
        return cls(**{name: _decode(ty, value[name]) for name, ty in hints.items()})


class ExecutionStatus(str, Enum):
    PREPARING = "preparing"
    RUNNING = "running"
    WAITING = "waiting"
    STOPPING = "stopping"
    FINISHING = "finishing"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"


class InvocationStatus(str, Enum):
    PREPARING = "preparing"
    RUNNING = "running"
    WAITING = "waiting"
    STOPPING = "stopping"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"


class InputStatus(str, Enum):
    QUEUED = "queued"
    APPLIED = "applied"
    NOT_APPLIED = "not_applied"


@dataclass(frozen=True)
class ResourceRef(DTO):
    uri: str
    media_type: str
    name: str | None


@dataclass(frozen=True)
class TextContent(DTO):
    text: str
    type: Literal["text"] = "text"


@dataclass(frozen=True)
class JsonContent(DTO):
    value: Any
    type: Literal["json"] = "json"


@dataclass(frozen=True)
class ResourceContent(DTO):
    resource: ResourceRef
    type: Literal["resource"] = "resource"


ContentBlock = TextContent | JsonContent | ResourceContent


@dataclass(frozen=True)
class AgentTarget(DTO):
    agent_id: str
    type: Literal["agent"] = "agent"


@dataclass(frozen=True)
class WorkflowTarget(DTO):
    workflow_id: str
    type: Literal["workflow"] = "workflow"


@dataclass(frozen=True)
class PluginPlanTarget(DTO):
    plan_id: str
    config: Any
    type: Literal["plugin_plan"] = "plugin_plan"


PlanTarget = AgentTarget | WorkflowTarget | PluginPlanTarget


@dataclass(frozen=True)
class UserInfo(DTO):
    user_id: str
    name: str | None


@dataclass(frozen=True)
class WorkspaceFileView(DTO):
    path: str
    content: str | None


@dataclass(frozen=True)
class WorkspaceBinding(DTO):
    root: str
    project_files: list[WorkspaceFileView]


@dataclass(frozen=True)
class ExecutionOptions(DTO):
    mode: Literal["todo"] | None = None

    @classmethod
    def from_dict(cls, value: dict) -> ExecutionOptions:
        if set(value) - {"mode"}:
            raise ValueError("Unknown execution options")
        return cls(_decode(Literal["todo"] | None, value.get("mode")))


@dataclass(frozen=True)
class TuiBinding(DTO):
    client_id: str
    type: Literal["tui"] = "tui"


@dataclass(frozen=True)
class LarkBinding(DTO):
    app_id: str
    chat_id: str
    thread_id: str | None
    type: Literal["lark"] = "lark"


@dataclass(frozen=True)
class SdkBinding(DTO):
    client_id: str
    type: Literal["sdk"] = "sdk"


ChannelBinding = TuiBinding | LarkBinding | SdkBinding


@dataclass(frozen=True)
class SessionConfig(DTO):
    session_id: str
    name: str
    lifecycle: Literal["open", "archived"]
    created_at: str
    updated_at: str
    target: PlanTarget
    workspace: WorkspaceBinding
    options: ExecutionOptions
    channels: list[ChannelBinding]


@dataclass(frozen=True)
class DefinitionRef(DTO):
    target: PlanTarget
    source: str | None
    content_fingerprint: str


@dataclass(frozen=True)
class ExecutionSettings(DTO):
    target: PlanTarget
    workspace: str
    options: ExecutionOptions
    definitions: list[DefinitionRef]


@dataclass(frozen=True)
class ModelRef(DTO):
    provider: str
    model: str


@dataclass(frozen=True)
class InvocationView(DTO):
    invocation_id: str
    parent_invocation_id: str | None
    kind: Literal["plan", "runtime"]
    plan_id: str
    task_id: str | None
    runtime_id: str | None
    agent_id: str | None
    workflow_id: str | None
    node_id: str | None
    model: ModelRef | None
    status: InvocationStatus


@dataclass(frozen=True)
class InputRecord(DTO):
    request_id: str
    execution_id: str
    status: InputStatus
    reason: str | None
    received_at: str
    processed_at: str | None


@dataclass(frozen=True)
class ErrorInfo(DTO):
    code: str
    message: str


@dataclass(frozen=True)
class Succeeded(DTO):
    result: list[ContentBlock]
    type: Literal["succeeded"] = "succeeded"


@dataclass(frozen=True)
class Failed(DTO):
    error: ErrorInfo
    type: Literal["failed"] = "failed"


@dataclass(frozen=True)
class Cancelled(DTO):
    reason: str
    type: Literal["cancelled"] = "cancelled"


ExecutionOutcome = Succeeded | Failed | Cancelled


@dataclass(frozen=True)
class ExecutionView(DTO):
    execution_id: str
    target: PlanTarget
    effective: ExecutionSettings
    status: ExecutionStatus
    invocations: list[InvocationView]
    started_at: str | None
    ended_at: str | None
    outcome: ExecutionOutcome | None
    inputs: list[InputRecord]


class SessionMessageRole(str, Enum):
    USER = "user"
    ASSISTANT = "assistant"
    SUMMARY = "summary"


class UserMemoryCategory(str, Enum):
    USER_ATTRIBUTE = "user_attribute"
    PREFERENCE = "preference"
    OTHER = "other"


class UserMemoryConfidence(str, Enum):
    USER_STATED = "user_stated"
    USER_CONFIRMED = "user_confirmed"
    SYSTEM_INFERRED = "system_inferred"

class ContextStackPhase(str, Enum):
    DISPATCHED = "Dispatched"
    STARTED = "Started"
    SUCCEEDED = "Succeeded"
    FAILED = "Failed"
    CANCELLED = "Cancelled"
    CLEANUP_FINISHED = "CleanupFinished"


@dataclass(frozen=True)
class AnyTaskType(DTO):
    Any: str


TaskMeta = TaskMetadata


@dataclass(frozen=True)
class ContextStack(DTO):
    entry_id: int
    parent_entry_id: int | None
    sequence: int
    scope_id: str
    task_meta: TaskMeta
    phase: ContextStackPhase
    timestamp: str
    elapsed_ms: int | None
    message: str | None


@dataclass(frozen=True)
class ContextStackQuery(DTO):
    tree: bool = False
    task_id: str | None = None
    plan_id: str | None = None
    executor: str | None = None
    phase: ContextStackPhase | None = None
    after_sequence: int | None = None
    limit: int | None = None


@dataclass
class SessionMessage(DTO):
    role: SessionMessageRole
    content: str
    images: list[str] | None = None

    @classmethod
    def from_dict(cls, value: dict) -> SessionMessage:
        if not isinstance(value, dict):
            raise TypeError("Expected SessionMessage object")
        return super().from_dict({"images": None, **value})

    def to_dict(self) -> dict:
        result = {"role": self.role.value, "content": self.content}
        if self.images is not None:
            result["images"] = self.images
        return result


@dataclass
class UserMemory(DTO):
    id: int
    category: UserMemoryCategory
    content: str
    confidence: UserMemoryConfidence
    updated_at: str

    @classmethod
    def from_dict(cls, value: dict) -> UserMemory:
        result = super().from_dict(value)
        if not 0 <= result.id < (1 << 64):
            raise ValueError("Memory id must fit u64")
        return result


@dataclass(frozen=True)
class MemoryView(DTO):
    history: list[SessionMessage]
    history_offset: int
    summary: str | None
    user_memories: list[UserMemory]


@dataclass(frozen=True)
class ContextData(DTO):
    user_info: UserInfo
    session_config: SessionConfig
    memory_view: MemoryView
    latest_execution_view: ExecutionView | None


@dataclass(frozen=True)
class MemorySummary(DTO):
    history_loaded: bool
    history_offset: int
    history_count: int
    user_memories_loaded: bool
    user_memory_count: int
    project_files_loaded: bool


@dataclass(frozen=True)
class ExecutionSummary(DTO):
    execution_id: str
    status: ExecutionStatus
    started_at: str | None
    ended_at: str | None


@dataclass(frozen=True)
class ContextSummary(DTO):
    memory: MemorySummary
    latest_execution: ExecutionSummary | None


@dataclass(frozen=True)
class SessionSummary(DTO):
    user_info: UserInfo
    session_config: SessionConfig
    context: ContextSummary
    active_execution: str | None
