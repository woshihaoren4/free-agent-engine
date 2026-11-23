"""Capabilities belong to one PluginCall; public DTOs contain no capability handles."""
from __future__ import annotations
from typing import Any
from .engine import Engine
from .session import Session
from .types import ContextChildSpec, ContextData, SessionSummary, ContextStack, ContextStackQuery, encode


class Context:
    __slots__ = ("_host", "_binding", "_session_id", "_summary")

    def __init__(self, *, _host: Any, _data: dict[str, Any]):
        self._host = _host
        self._binding = dict(_data["binding"])
        self._session_id = _data.get("session_id")
        self._summary = SessionSummary.from_dict(_data["summary"]) if _data.get("summary") else None

    @property
    def execution_id(self) -> str | None:
        return self._binding["execution_id"]

    @property
    def invocation_id(self) -> str | None:
        return self._binding["invocation_id"]

    @property
    def session(self) -> Session | None:
        return Session(_host=self._host, _binding=self._binding, _session_id=self._session_id) if self._session_id else None

    async def _call(self, path: str, **params: Any) -> Any:
        return await self._host.call(path, {"access": self._host.access(self._binding), **encode(params)})

    async def read_summary(self) -> SessionSummary:
        return SessionSummary.from_dict(await self._host.call("/request/plugin/context/get", self._host.access(self._binding)))

    async def export_context(self) -> ContextData:
        return ContextData.from_dict(await self._host.call("/request/plugin/context/export", self._host.access(self._binding)))

    async def stacks(self, query: ContextStackQuery | None = None) -> list[ContextStack]:
        query = query or ContextStackQuery()
        if not isinstance(query, ContextStackQuery):
            raise TypeError("query must be ContextStackQuery")
        return [ContextStack.from_dict(record) for record in
                await self._call("/request/plugin/context/stacks", query=query)]

    async def get_engine(self) -> Engine:
        data = await self._host.call("/request/plugin/host/get_engine", self._host.access(self._binding))
        return Engine(_host=self._host, _engine_id=data["handle"])

    async def get_session(self) -> Session | None:
        data = await self._host.call("/request/plugin/host/get_session", self._host.access(self._binding))
        return Context(_host=self._host, _data=data).session if data else None

    async def fork(self, spec: ContextChildSpec) -> Context:
        if not isinstance(spec, ContextChildSpec):
            raise TypeError("spec must be ContextChildSpec")
        return Context(_host=self._host, _data=await self._call("/request/plugin/context/fork", spec=spec))

    async def cancel_local(self, reason: str = "Cancelled by plugin") -> None:
        await self._call("/request/plugin/context/cancel", reason=reason)

    async def cancel_execution(self, request_id: str) -> Any:
        if self.session is None:
            raise ValueError("Context has no execution")
        return await self.session.stop(request_id)
