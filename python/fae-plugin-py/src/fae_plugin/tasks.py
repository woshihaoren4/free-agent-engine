"""JSON task DTOs used by Plan and Runtime providers."""
from __future__ import annotations
from dataclasses import dataclass
from typing import Any, TypeAlias

JsonValue: TypeAlias = None | bool | int | float | str | list["JsonValue"] | dict[str, "JsonValue"]


def json_value(value: Any) -> JsonValue:
    from .types import json_value as validate
    return validate(value)

BUILTIN_TASK_TYPES = frozenset(("Tool", "Skill", "Mcp", "Plan", "Workflow", "Model", "Session", "Memory"))
TaskType = str | dict[str, str]


def task_type(value: Any) -> TaskType:
    if isinstance(value, str) and value in BUILTIN_TASK_TYPES:
        return value
    if (isinstance(value, dict) and set(value) == {"Any"}
            and isinstance(value["Any"], str) and value["Any"].strip()
            and value["Any"].strip() == value["Any"]
            and value["Any"] not in BUILTIN_TASK_TYPES
            and not any(ord(char) < 32 for char in value["Any"])):
        return dict(value)
    raise ValueError("Invalid TaskType")


def task_types(value: Any) -> list[str]:
    if (not isinstance(value, list) or not value
            or any(not isinstance(name, str) or not name or name.strip() != name
                   or any(ord(char) < 32 for char in name) for name in value)
            or len(set(value)) != len(value)):
        raise ValueError("task_types must be a nonempty array of unique names")
    return list(value)


@dataclass(frozen=True)
class TaskMetadata:
    id: str
    ty: TaskType
    executor: str | None = None
    plan_run_id: str | None = None
    publisher: str | None = None
    origin_plan_id: str | None = None
    target_plan_id: str | None = None

    def __post_init__(self) -> None:
        task_type(self.ty)
        for field in ("id", "executor", "plan_run_id", "publisher", "origin_plan_id", "target_plan_id"):
            value = getattr(self, field)
            if (field == "id" or value is not None) and (
                    not isinstance(value, str) or not value.strip() or value.strip() != value):
                raise ValueError(f"Invalid task {field}")
        if self.target_plan_id is not None and self.ty != "Plan":
            raise ValueError("target_plan_id requires a Plan task")

    def to_dict(self) -> dict[str, Any]:
        return {field: getattr(self, field) for field in self.__dataclass_fields__}

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> TaskMetadata:
        if not isinstance(value, dict):
            raise TypeError("Expected task metadata")
        return cls(**{key: None if key in ("executor", "plan_run_id", "publisher") and item == "" else item
                      for key, item in value.items()})


@dataclass
class TaskRequest:
    meta: TaskMetadata
    req: JsonValue
    ctx: Any = None

    def to_dict(self) -> dict[str, Any]:
        return {"meta": self.meta.to_dict(), "context": getattr(self.ctx, "reference", self.ctx), "payload": json_value(self.req)}

    @classmethod
    def from_dict(cls, value: dict[str, Any], *, ctx: Any = None) -> TaskRequest:
        _check_dto(value)
        return cls(TaskMetadata.from_dict(value["meta"]), json_value(value["payload"]), ctx)


@dataclass
class TaskResponse:
    meta: TaskMetadata
    resp: JsonValue
    ctx: Any = None

    def to_dict(self) -> dict[str, Any]:
        return {"meta": self.meta.to_dict(), "context": getattr(self.ctx, "reference", self.ctx), "payload": json_value(self.resp)}

    @classmethod
    def from_dict(cls, value: dict[str, Any], *, ctx: Any = None) -> TaskResponse:
        _check_dto(value)
        return cls(TaskMetadata.from_dict(value["meta"]), json_value(value["payload"]), ctx)


def _check_dto(value: Any) -> None:
    if not isinstance(value, dict) or set(value) != {"meta", "context", "payload"}:
        raise ValueError("Expected complete task DTO")


@dataclass(frozen=True)
class PlanNext:
    kind: str
    tasks: tuple[TaskRequest, ...] = ()

    def __post_init__(self) -> None:
        if self.kind not in ("tasks", "end") or (self.kind == "end" and self.tasks):
            raise ValueError("Invalid PlanNext")
        if any(not isinstance(task, TaskRequest) for task in self.tasks):
            raise TypeError("PlanNext tasks must be TaskRequest")

    @classmethod
    def Tasks(cls, tasks: list[TaskRequest]) -> PlanNext:
        return cls("tasks", tuple(tasks))

    @classmethod
    def End(cls) -> PlanNext:
        return cls("end")

    def to_dict(self) -> dict[str, Any]:
        return {"kind": "end"} if self.kind == "end" else {
            "kind": "tasks", "tasks": [task.to_dict() for task in self.tasks]}
