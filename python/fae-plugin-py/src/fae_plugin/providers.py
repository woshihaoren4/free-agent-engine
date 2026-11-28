"""Plan and Runtime contracts with task-local capability bindings."""
from __future__ import annotations
import asyncio
import threading
from contextvars import ContextVar
from dataclasses import dataclass
from typing import Any
from .tasks import PlanNext, TaskRequest, TaskResponse, TaskType
from .types import JsonValue


class RuntimeNoSupport(Exception):
    """Raised only before accepting an unsupported operation."""


class MissingPlanHandler(Exception):
    pass


@dataclass(frozen=True)
class ProviderCall:
    owner: Any
    ctx: Any
    rt: Any
    continuation: Any


_call: ContextVar[ProviderCall | None] = ContextVar("fae_provider_call", default=None)


def call_owner() -> Any:
    try:
        return asyncio.current_task()
    except RuntimeError:
        return threading.current_thread()


def current_call() -> ProviderCall:
    call = _call.get()
    if call is None or call.owner is not call_owner():
        raise RuntimeError("Provider capabilities are unavailable outside their call")
    return call


class Plan:
    def id(self) -> str:
        raise NotImplementedError

    @property
    def env(self) -> JsonValue:
        return self._env

    @property
    def ctx(self) -> Any:
        return current_call().ctx

    @property
    def rt(self) -> Any:
        return current_call().rt

    async def init(self) -> PlanNext:
        raise MissingPlanHandler("MissingPlanHandler: init")

    async def next(self, task_result: TaskResponse) -> PlanNext:
        raise MissingPlanHandler("MissingPlanHandler: next")

    async def abort(self, code: int, error: str) -> None:
        pass


class ChainPlan(Plan):
    @property
    def next_plan(self) -> Any:
        return current_call().continuation

    async def init(self) -> PlanNext:
        return await self.next_plan.init()

    async def next(self, task_result: TaskResponse) -> PlanNext:
        return await self.next_plan.next(task_result)

    async def abort(self, code: int, error: str) -> None:
        await self.next_plan.abort(code, error)


class Runtime:
    def id(self) -> str:
        raise NotImplementedError

    def tys(self) -> list[str]:
        raise NotImplementedError

    @property
    def ctx(self) -> Any:
        return current_call().ctx

    async def select(self, ty: TaskType, cond: JsonValue) -> JsonValue:
        raise RuntimeNoSupport

    async def exec(self, task: TaskRequest) -> TaskResponse:
        raise RuntimeNoSupport

    async def kill(self, task_id: str) -> None:
        pass

    async def exit(self) -> None:
        pass


class ChainRuntime(Runtime):
    @property
    def next_runtime(self) -> Any:
        return current_call().continuation

    async def select(self, ty: TaskType, cond: JsonValue) -> JsonValue:
        return await self.next_runtime.select(ty, cond)

    async def exec(self, task: TaskRequest) -> TaskResponse:
        return await self.next_runtime.exec(task)

    async def kill(self, task_id: str) -> None:
        await self.next_runtime.kill(task_id)

    async def exit(self) -> None:
        await self.next_runtime.exit()
