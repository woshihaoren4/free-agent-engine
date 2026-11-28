"""Short-lived capabilities for one authenticated provider method."""
from __future__ import annotations
import asyncio
from dataclasses import dataclass
from typing import Any
from .tasks import TaskRequest, TaskResponse
from .types import json_value


@dataclass
class ProviderOwner:
    host: Any
    access: dict[str, Any]
    session: Any
    deadline: float
    active: bool = True
    generation: str | None = None

    def remaining(self) -> float:
        return max(0.0, self.deadline - asyncio.get_running_loop().time())

    def check(self) -> None:
        if not self.active or self.remaining() <= 0:
            raise RuntimeError("Provider capability expired")


class ProviderContext:
    def __init__(self, owner: ProviderOwner, reference: dict[str, str]):
        self.owner, self.reference = owner, dict(reference)

    async def complete(self, result: Any, *, streams: tuple[str, ...] | list[str] = ()) -> None:
        binding = {"resources": list(streams)} if streams else {}
        await self.owner.host.provider_call(self.owner, "context/complete", json_value(result), binding)

    async def get(self) -> Any:
        return await self.owner.host.provider_call(self.owner, "context/get", None, {"context": self.reference})

    async def stream_next(self, stream_id: str) -> Any:
        """Consume a boundary stream; a fresh call capability is used each time."""
        return await self.owner.host.provider_call(self.owner, "host/stream/next", stream_id,
                                                   {"context": self.reference})

    async def stream_close(self, stream_id: str) -> None:
        await self.owner.host.provider_call(self.owner, "host/stream/close", stream_id,
                                             {"context": self.reference})

    async def stream(self, iterator: Any) -> Any:
        """Register an async iterator and return a bounded host stream reference."""
        self.owner.check()
        worker = self.owner.host.provider_worker
        stream_id = worker.register_provider_stream(self.owner, self.reference, iterator)
        try:
            return await self.owner.host.provider_call(self.owner, "host/stream/register", stream_id,
                                                       {"context": self.reference})
        except BaseException:
            await worker.close_provider_stream(stream_id)
            raise


class ProviderRuntime:
    def __init__(self, owner: ProviderOwner, ctx: ProviderContext):
        self.owner, self.ctx = owner, ctx

    async def select(self, runtime_id: str, ty: Any, cond: Any, *, ctx: ProviderContext | None = None) -> Any:
        context = ctx or self.ctx
        return await self.owner.host.provider_call(self.owner, "host/runtime/select", json_value(cond),
            {"context": context.reference, "ty": ty}, runtime=runtime_id)

    async def exec(self, task: TaskRequest) -> TaskResponse:
        context = task.ctx or self.ctx
        value = await self.owner.host.provider_call(self.owner, "host/runtime/exec", json_value(task.req),
            {"context": context.reference, "meta": task.meta.to_dict()}, runtime=task.meta.executor)
        return TaskResponse(task.meta, value, task.ctx)

    async def call_plan(self, plan_id: str, env: Any) -> Any:
        return await self.owner.host.provider_call(self.owner, "host/plan/call", json_value(env), {}, plan=plan_id)
