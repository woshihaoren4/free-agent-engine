"""Capabilities are scoped to one host invocation, never to a global current call."""
from __future__ import annotations
import asyncio
from typing import Any
from .engine import Engine
from .errors import InvalidHookNext, StateViewClosed
from .session import Session
from .types import (
    ContextChildSpec, ContextFailure, ContextOutput, ContextScope, ContextSession,
    ContextStack, ContextStackDraft, ContextStackPhase, ContextStackQuery,
    ContextState, ContextStateKind, ContextStateUpdate, ContextUser,
    SingleAgentSource, TaskMeta, TaskType, TaskTypeKind, TimerExecutionBinding,
    TimerOwner, TodoPlanScope, encode,
)


def decode_state(value: dict[str, Any], host: Any) -> ContextState:
    kind = ContextStateKind(value["kind"])
    if kind == ContextStateKind.SUCCEEDED:
        out = value["output"]
        output = ContextOutput(out.get("json"), _host=host, _handle=out.get("handle"))
        return ContextState(kind, output=output)
    if kind == ContextStateKind.FAILED:
        return ContextState(kind, error=ContextFailure(**value["error"]))
    if kind == ContextStateKind.CANCELLED:
        return ContextState(kind, reason=value["reason"])
    return ContextState(kind)


class ContextStateView:
    def __init__(self, host: Any, handle: str):
        self._host = host
        self._registration = asyncio.create_task(host.call("state.subscribe", {"handle": handle}))
        self._closed = False
        self._changing = False

    async def _params(self) -> dict[str, Any]:
        if self._closed:
            raise StateViewClosed("State view is closed")
        return {"handle": (await self._registration)["handle"]}

    async def current(self) -> ContextState:
        return decode_state(await self._host.call("state.current", await self._params()), self._host)

    async def changed(self) -> None:
        if self._changing:
            raise InvalidHookNext("Only one changed operation may be in flight")
        self._changing = True
        try:
            await self._host.call("state.changed", await self._params())
        finally:
            self._changing = False

    async def aclose(self) -> None:
        if self._closed:
            return
        self._closed = True
        registration = await self._registration
        await self._host.call("state.close", {"handle": registration["handle"]})


class Context:
    __slots__ = ("_host", "_handle", "_identity", "_session")

    def __init__(self, *, _host: Any, _data: dict[str, Any]):
        self._host = _host
        self._handle = _data["handle"]
        self._refresh(_data)

    def _refresh(self, data: dict[str, Any]) -> None:
        self._identity = dict(data.get("identity", {}))
        session = data.get("session")
        self._session = (Session(_host=self._host, _identity=session["identity"],
                                 _handle=session["handle"]) if session else None)

    @property
    def agent_id(self) -> str | None:
        return self._identity.get("agent_id")

    @property
    def user_id(self) -> str | None:
        return self._identity.get("user_id")

    @property
    def plan_id(self) -> str | None:
        return self._identity.get("plan_id")

    @property
    def task_id(self) -> str | None:
        return self._identity.get("task_id")

    @property
    def turn_id(self) -> int | None:
        return self._identity.get("turn_id")

    @property
    def scope_id(self) -> str | None:
        return self._identity.get("scope_id")

    @property
    def run_id(self) -> str | None:
        return self._identity.get("run_id")

    @property
    def session(self) -> Session | None:
        return self._session

    async def _call(self, method: str, **params: Any) -> Any:
        return await self._host.call(method, {"handle": self._handle, **encode(params)})

    async def get_engine(self) -> Engine:
        data = await self._call("context.get_engine")
        return Engine(_host=self._host, _engine_id=data["engine_id"])

    async def get_session(self) -> Session | None:
        data = await self._call("context.get_session")
        return Session(_host=self._host, _identity=data["identity"], _handle=data["handle"]) if data else None

    async def scope(self) -> ContextScope:
        data = await self._call("context.scope")
        data["user"] = ContextUser(**data["user"])
        if data.get("session"):
            item = data["session"]
            proxy = Session(_host=self._host, _identity=item["identity"], _handle=item["handle"])
            data["session"] = ContextSession(proxy.session_id, proxy)
        if data.get("todo"):
            item = dict(data["todo"])
            item["_handle"] = item.pop("handle")
            data["todo"] = TodoPlanScope(**item)
        if data.get("timer_execution"):
            timer = data["timer_execution"]
            data["timer_execution"] = TimerExecutionBinding(
                TimerOwner(**timer["owner"]), SingleAgentSource(**timer["source"]), timer["workspace"])
        return ContextScope(**data)

    async def bind_scope(self, scope: ContextScope) -> None:
        if not isinstance(scope, ContextScope):
            raise TypeError("scope must be ContextScope")
        self._refresh(await self._call("context.bind_scope", scope=scope))

    async def fork(self, spec: ContextChildSpec) -> Context:
        if not isinstance(spec, ContextChildSpec):
            raise TypeError("spec must be ContextChildSpec")
        return Context(_host=self._host, _data=await self._call("context.fork", spec=spec))

    def state(self) -> ContextStateView:
        return ContextStateView(self._host, self._handle)

    async def set_state(self, state: ContextState) -> ContextStateUpdate:
        if not isinstance(state, ContextState):
            raise TypeError("state must be ContextState")
        return ContextStateUpdate(await self._call("context.set_state", state=state))

    async def append_stack(self, entry: ContextStackDraft) -> int:
        if not isinstance(entry, ContextStackDraft):
            raise TypeError("entry must be ContextStackDraft")
        return await self._call("context.append_stack", entry=entry)

    async def stacks(self, query: ContextStackQuery) -> list[ContextStack]:
        if not isinstance(query, ContextStackQuery):
            raise TypeError("query must be ContextStackQuery")
        result = []
        for item in await self._call("context.stacks", query=query):
            meta = item["task_meta"]
            meta["ty"] = TaskType(TaskTypeKind(meta["ty"]["kind"]), meta["ty"].get("value"))
            item["task_meta"] = TaskMeta(**meta)
            item["phase"] = ContextStackPhase(item["phase"])
            result.append(ContextStack(**item))
        return result

    async def cancel(self, reason: str = "Cancelled by plugin") -> None:
        await self._call("context.cancel", reason=reason)

    async def cancel_run(self, reason: str = "Cancelled by plugin") -> None:
        await self._call("context.cancel_run", reason=reason)
