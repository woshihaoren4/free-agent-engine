from typing import Any
from .types import MessageReceipt


class Session:
    __slots__ = ("_host", "_identity", "_handle")

    def __init__(self, *, _host: Any, _identity: dict[str, Any], _handle: str):
        self._host = _host
        self._identity = dict(_identity)
        self._handle = _handle

    @property
    def session_id(self) -> str:
        return self._identity["session_id"]

    @property
    def user_id(self) -> str:
        return self._identity["user_id"]

    @property
    def agent_id(self) -> str | None:
        return self._identity.get("agent_id")

    @property
    def turn_id(self) -> int | None:
        return self._identity.get("turn_id")

    @property
    def run_id(self) -> str | None:
        return self._identity.get("run_id")

    async def send(self, text: str) -> MessageReceipt:
        if not isinstance(text, str):
            raise TypeError("Session.send requires text")
        if not text.strip():
            raise ValueError("Session message must not be blank")
        result = await self._host.call("session.send", {"handle": self._handle, "text": text})
        return MessageReceipt(**result)

    def _wire_reference(self) -> dict[str, str]:
        return {"handle": self._handle}
