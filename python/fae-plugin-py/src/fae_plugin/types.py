"""Public business DTOs and native provider values for API 0.2.0."""
from __future__ import annotations
import json
import math
from dataclasses import dataclass, fields, is_dataclass
from enum import Enum
from typing import Any, AsyncIterator, TypeAlias
from .data import *

API_VERSION = "0.2.0"
JsonValue: TypeAlias = None | bool | int | float | str | list["JsonValue"] | dict[str, "JsonValue"]


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


class RegistrationMode(str, Enum):
    CHAIN = "chain"
    DIRECT = "direct"


@dataclass(frozen=True)
class ContextChildSpec:
    operation: str

    def __post_init__(self) -> None:
        if not isinstance(self.operation, str) or not self.operation.strip() or len(self.operation) > 256:
            raise ValueError("Child operation must be a nonempty local name")


@dataclass(frozen=True)
class OutputReceipt:
    output_id: str


class NativeObject:
    """Provider-specific JSON remains lossless and separate from public DTOs."""
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
        return json_value(self._data)


class ModelRequest(NativeObject):
    pass


class ToolRequest(NativeObject):
    def arguments_json(self) -> JsonValue:
        return json_value(json.loads(self.arguments))


class ToolResponse:
    def __init__(self, result: str | None = None, *, _reference: str | None = None):
        self.result, self._reference = result, _reference

    @classmethod
    def completed(cls, result: str) -> ToolResponse:
        if not isinstance(result, str):
            raise TypeError("Tool result must be a string")
        return cls(result)


class ModelResponse:
    def __init__(self, value: dict[str, Any] | AsyncIterator[dict[str, Any]], *, streaming: bool):
        self.value, self.is_streaming = value, streaming

    @classmethod
    def completed(cls, value: dict[str, Any]) -> ModelResponse:
        if not isinstance(value, dict):
            raise TypeError("Model completion must be an OpenAI response object")
        return cls(json_value(value), streaming=False)

    @classmethod
    def streaming(cls, chunks: AsyncIterator[dict[str, Any]]) -> ModelResponse:
        if not hasattr(chunks, "__aiter__"):
            raise TypeError("Model stream must be an async iterator")
        return cls(chunks, streaming=True)

    def __aiter__(self) -> AsyncIterator[dict[str, Any]]:
        if not self.is_streaming:
            raise TypeError("Completed response is not a stream")
        return self.value.__aiter__()


def encode(value: Any) -> Any:
    if isinstance(value, SessionMessage):
        return value.to_dict()
    if isinstance(value, NativeObject):
        return value.to_dict()
    if isinstance(value, Enum):
        return value.value
    if is_dataclass(value):
        return {f.name: encode(getattr(value, f.name)) for f in fields(value) if not f.name.startswith("_")}
    if isinstance(value, (list, tuple)):
        return [encode(v) for v in value]
    if isinstance(value, dict):
        return {k: encode(v) for k, v in value.items()}
    return json_value(value)
