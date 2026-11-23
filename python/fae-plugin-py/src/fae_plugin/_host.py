import asyncio
import itertools
from typing import Any
from ._protocol import request
from .errors import HandleExpired, HostCallTimeout, from_wire


class Host:
    def __init__(self, writer: Any):
        self.writer = writer
        self.pending: dict[str, tuple[dict[str, Any], asyncio.Future[Any]]] = {}
        self.ids = itertools.count()

    async def call(self, call: Any, member: str, path: str, params: dict[str, Any],
                   request_id: str | None = None) -> Any:
        call.check()
        if len(self.pending) >= 256:
            raise from_wire({"code": 409, "msg": "Host request capacity exceeded"})
        request_id = request_id or f"host-{next(self.ids)}"
        if request_id in self.pending:
            raise ValueError("request_id already in flight")
        reference = call.params["contexts"][member]
        message = request(request_id, path, params, reference.get("session_id"))
        future = asyncio.get_running_loop().create_future()
        self.pending[request_id] = (message, future)
        try:
            await self.writer.send(message, control=True)
            try:
                return await asyncio.wait_for(future, call.remaining())
            except asyncio.TimeoutError:
                raise HostCallTimeout("Host request exceeded call deadline") from None
        finally:
            self.pending.pop(request_id, None)

    def response(self, frame: dict[str, Any]) -> None:
        pending = self.pending.get(frame["request_id"])
        if pending is None:
            return
        message, future = pending
        if future.done():
            return
        if frame["path"] != message["path"].replace("/request/", "/response/", 1) or frame.get("session_id") != message.get("session_id"):
            future.set_exception(ValueError("Response binding mismatch"))
        elif frame["payload"]["code"] != 0:
            future.set_exception(from_wire(frame["payload"]))
        else:
            future.set_result(frame["payload"]["data"])

    def close(self) -> None:
        for _, future in self.pending.values():
            if not future.done():
                future.set_exception(HandleExpired("Host connection closed"))


class BoundHost:
    def __init__(self, host: Host, call: Any, member: str):
        self.host, self.call_owner, self.member = host, call, member

    async def call(self, path: str, params: dict[str, Any], request_id: str | None = None) -> Any:
        return await self.host.call(self.call_owner, self.member, path, params, request_id)

    def access(self, binding: dict[str, Any] | None = None) -> dict[str, Any]:
        return {"call_id": self.call_owner.id, "member": self.member,
                "binding": binding or self.call_owner.params["contexts"][self.member]["binding"]}
