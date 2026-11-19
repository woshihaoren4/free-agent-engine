import asyncio
import itertools
from typing import Any
from .errors import HandleExpired, HookCancelled, HostCallTimeout, from_wire


class Host:
    def __init__(self, writer: Any):
        self.writer = writer
        self.pending: dict[str, asyncio.Future[Any]] = {}
        self.ids = itertools.count()

    async def call(self, invocation: Any, member: str, method: str, params: dict[str, Any]) -> Any:
        invocation.check()
        if len(self.pending) >= 256:
            raise from_wire({"code": "plugin_busy", "message": "Host request capacity exceeded",
                             "resource": "host_requests"})
        request_id = f"host-{next(self.ids)}"
        future = asyncio.get_running_loop().create_future()
        self.pending[request_id] = future
        try:
            await self.writer.send({"id": request_id, "method": "host.call", "params": {
                "call_id": invocation.id, "member": member, "method": method, "params": params,
            }}, control=True)
            try:
                value = await asyncio.wait_for(future, invocation.remaining())
            except asyncio.TimeoutError:
                raise HostCallTimeout("Host request exceeded invocation deadline") from None
            if isinstance(value, dict) and value.get("_invocation_terminal"):
                invocation.terminal = True
                value = value.get("result")
            return value
        finally:
            self.pending.pop(request_id, None)

    def response(self, frame: dict[str, Any]) -> None:
        future = self.pending.get(frame.get("id", ""))
        if future is not None and not future.done():
            if "error" in frame:
                future.set_exception(from_wire(frame["error"]))
            else:
                future.set_result(frame.get("result"))

    def close(self) -> None:
        for future in self.pending.values():
            if not future.done():
                future.set_exception(HandleExpired("Host connection closed"))


class BoundHost:
    def __init__(self, host: Host, invocation: Any, member: str):
        self.host, self.invocation, self.member = host, invocation, member

    async def call(self, method: str, params: dict[str, Any]) -> Any:
        return await self.host.call(self.invocation, self.member, method, params)
