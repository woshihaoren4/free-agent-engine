from typing import Any
from .types import ContentBlock, InputRecord, OutputReceipt, encode


class Session:
    __slots__ = ("_host", "_binding", "_session_id")

    def __init__(self, *, _host: Any, _binding: dict[str, Any], _session_id: str):
        self._host = _host
        self._binding = dict(_binding)
        self._session_id = _session_id

    @property
    def session_id(self) -> str:
        return self._session_id

    @property
    def execution_id(self) -> str:
        return self._binding["execution_id"]

    @property
    def invocation_id(self) -> str:
        return self._binding["invocation_id"]

    async def send(self, content: list[ContentBlock]) -> OutputReceipt:
        result = await self._host.call("/request/plugin/output/send",
            {"access": self._host.access(self._binding), "content": encode(content)})
        return OutputReceipt(**result)

    async def steer(self, content: list[ContentBlock], request_id: str) -> dict[str, Any]:
        return await self._host.call("/request/session/execution/steer",
            {"execution_id": self.execution_id, "input": encode(content)}, request_id)

    async def stop(self, request_id: str, reason: str | None = None) -> dict[str, Any]:
        return await self._host.call("/request/session/execution/stop",
            {"execution_id": self.execution_id, "reason": reason}, request_id)

    async def input_record(self, request_id: str) -> InputRecord | None:
        data = await self._host.call("/request/session/execution/get",
            {"execution_id": self.execution_id, "input_request_id": request_id})
        record = data["acceptance"]["input"]
        return InputRecord.from_dict(record) if record else None
