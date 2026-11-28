"""Worker-owned provider factories, instance groups and local continuations."""
from __future__ import annotations
import asyncio
import copy
import inspect
from collections import OrderedDict
from dataclasses import dataclass, field
from typing import Any
from .providers import (
    ChainPlan, ChainRuntime, MissingPlanHandler, ProviderCall, RuntimeNoSupport,
    _call, call_owner,
)
from .registration import Instance
from .tasks import PlanNext, TaskRequest, TaskResponse, task_type
from .types import json_value


class Continuation:
    def __init__(self, method: str, callback: Any):
        self.method = method
        self.callback = callback
        self.owner = call_owner()
        self.used = False
        self.active = True

    def __getattr__(self, method: str) -> Any:
        async def proceed(*args: Any) -> Any:
            if not self.active or self.owner is not call_owner() or self.used or method != self.method:
                raise RuntimeError("Invalid provider continuation")
            self.used = True
            return await self.callback(*args)
        return proceed


@dataclass
class PlanGroup:
    entry_id: str
    objects: list[Any]
    lock: asyncio.Lock = field(default_factory=asyncio.Lock)
    aborted: set[int] = field(default_factory=set)
    started: bool = False
    released: bool = False


class ProviderRegistry:
    def __init__(self, records: list[Instance]):
        self.plans = {record.mount["id"]: record for record in records if record.mount["point"] == "Plan"}
        self.runtimes: dict[str, list[Instance]] = {}
        for record in records:
            if record.mount["point"] == "Runtime":
                self.runtimes.setdefault(record.mount["id"], []).append(record)
        self.groups: dict[str, PlanGroup] = {}
        self.runtime_objects: dict[tuple[str, str], list[Any]] = {}
        self.retired: OrderedDict[tuple[str, str], None] = OrderedDict()
        self.released: OrderedDict[str, str] = OrderedDict()
        self.thread_slots = asyncio.Semaphore(8)
        self.accepting = True

    def descriptors(self) -> dict[str, Any]:
        return {
            "plans": list(self.plans),
            "runtimes": {
                id: {"task_types": records[0].mount["task_types"], "methods": [
                    method for method in ("select", "exec")
                    if any(getattr(type(record.object), method) is not getattr(record.base, method)
                           for record in records)]}
                for id, records in self.runtimes.items()
            },
        }

    def create(self, plan_id: str, instance_id: str, env: Any) -> str:
        if not self.accepting or len(self.groups) >= 1024:
            raise RuntimeError("Plan instance capacity exceeded")
        if not isinstance(instance_id, str) or not instance_id or instance_id in self.groups or instance_id in self.released:
            raise ValueError("Invalid or reused Plan instance ID")
        env = json_value(env)
        objects = []
        current = plan_id
        while current is not None:
            record = self.plans[current]
            obj = record.object()
            if obj.id() != current:
                raise ValueError("Plan id() changed after registration")
            obj._env = copy.deepcopy(env)
            objects.append(obj)
            current = record.mount.get("next_plan_id")
        self.groups[instance_id] = PlanGroup(plan_id, objects)
        return instance_id

    def group(self, plan_id: str, instance_id: str) -> PlanGroup:
        group = self.groups[instance_id]
        if group.entry_id != plan_id or group.released:
            raise ValueError("Plan instance does not belong to provider")
        return group

    async def plan(self, plan_id: str, instance_id: str, method: str, args: tuple[Any, ...],
                   *, ctx: Any, rt: Any = None) -> Any:
        group = self.group(plan_id, instance_id)
        async with group.lock:
            if group.released:
                raise ValueError("Plan instance released")
            if method == "init":
                if group.started:
                    raise RuntimeError("Plan already initialized")
                group.started = True
            elif method == "next" and (not group.started or group.aborted):
                raise RuntimeError("Plan is not running")
            elif method not in ("next", "abort"):
                raise ValueError("Invalid Plan method")
            if method == "abort":
                await self._abort(group, args, ctx)
                return None
            result = await self._invoke(group.objects, method, args, ctx, rt)
            if not isinstance(result, PlanNext):
                raise TypeError("Plan must return PlanNext")
            # Validate all JSON before handing any tasks to the host.
            json_value(result.to_dict())
            return result.to_dict()

    async def _abort(self, group: PlanGroup, args: tuple[Any, ...], ctx: Any) -> None:
        first_error: BaseException | None = None
        for index in range(len(group.objects)):
            if index in group.aborted:
                continue
            try:
                await self._invoke(group.objects, "abort", args, ctx, None,
                                   index=index, visited=group.aborted)
            except Exception as error:
                first_error = first_error or error
        if first_error is not None:
            raise first_error

    async def release(self, plan_id: str, instance_id: str) -> None:
        if instance_id in self.released:
            if self.released[instance_id] != plan_id:
                raise ValueError("Plan instance does not belong to provider")
            return
        group = self.groups.get(instance_id)
        if group is not None:
            if group.entry_id != plan_id:
                raise ValueError("Plan instance does not belong to provider")
            async with group.lock:
                group.released = True
                group.objects.clear()
                self.groups.pop(instance_id, None)
                self._remember_release(plan_id, instance_id)
        else:
            # The host may release after abandoning create, without knowing
            # whether the Worker received it. Block a late create for this ID.
            self._remember_release(plan_id, instance_id)

    def _remember_release(self, plan_id: str, instance_id: str) -> None:
        self.released[instance_id] = plan_id
        while len(self.released) > 1024:
            self.released.popitem(last=False)

    async def runtime(self, runtime_id: str, method: str, args: tuple[Any, ...], *, ctx: Any,
                      generation: str = "local") -> Any:
        if not self.accepting and method not in ("kill", "exit"):
            raise RuntimeError("Runtime is shutting down")
        records = self.runtimes[runtime_id]
        if method not in ("select", "exec", "kill", "exit"):
            raise ValueError("Invalid Runtime method")
        if method in ("select", "exec"):
            ty = task_type(args[0] if method == "select" else args[0].meta.ty)
            name = ty if isinstance(ty, str) else ty["Any"]
            if name not in records[0].mount["task_types"]:
                raise RuntimeNoSupport
        key = (generation, runtime_id)
        if key in self.retired:
            if method in ("exit", "kill"):
                return None
            raise RuntimeError("Runtime generation retired")
        objects = self.runtime_objects.get(key)
        if objects is None:
            if method == "kill":
                return None
            if len(self.runtime_objects) >= 1024:
                raise RuntimeError("Runtime generation capacity exceeded")
            objects = [type(record.object)() for record in records]
            for obj, record in zip(objects, records):
                if obj.id() != runtime_id or set(obj.tys()) != set(record.mount["task_types"]):
                    raise ValueError("Runtime identity changed after registration")
            self.runtime_objects[key] = objects
        if method == "exit":
            self.retired[key] = None
            while len(self.retired) > 1024:
                self.retired.popitem(last=False)
            self.runtime_objects.pop(key, None)
        if method in ("kill", "exit"):
            visited: set[int] = set()
            first_error: Exception | None = None
            for index in range(len(objects)):
                if index not in visited:
                    try:
                        await self._invoke(objects, method, args, ctx, None, index=index, visited=visited)
                    except Exception as error:
                        first_error = first_error or error
            if first_error:
                raise first_error
            return None
        result = await self._invoke(objects, method, args, ctx, None)
        if method == "exec":
            if not isinstance(result, TaskResponse):
                raise TypeError("Runtime exec must return TaskResponse")
            task: TaskRequest = args[0]
            if result.meta != task.meta or result.ctx is not task.ctx:
                raise ValueError("Runtime response changed task identity or Context")
            return json_value(result.resp)
        return json_value(result)

    async def _invoke(self, objects: list[Any], method: str, args: tuple[Any, ...],
                      ctx: Any, rt: Any, *, index: int = 0, visited: set[int] | None = None) -> Any:
        if index == len(objects):
            if method in ("kill", "exit", "abort"):
                return None
            if method in ("init", "next"):
                raise MissingPlanHandler(method)
            raise RuntimeNoSupport
        if visited is not None:
            if index in visited:
                return None
            visited.add(index)
        continuation = Continuation(method, lambda *forwarded: self._invoke(
            objects, method, forwarded, ctx, rt, index=index + 1, visited=visited))
        function = getattr(objects[index], method)
        try:
            if inspect.iscoroutinefunction(function):
                token = _call.set(ProviderCall(call_owner(), ctx, rt, continuation))
                try:
                    return await function(*args)
                finally:
                    _call.reset(token)
            def run() -> Any:
                token = _call.set(ProviderCall(call_owner(), ctx, rt, None))
                try:
                    return function(*args)
                finally:
                    _call.reset(token)
            await self.thread_slots.acquire()
            # Cancellation cannot stop a Python thread. Keep its slot until the
            # actual function finishes, so repeated cancelled calls stay bounded.
            work = asyncio.create_task(asyncio.to_thread(run))
            work.add_done_callback(lambda _: self.thread_slots.release())
            return await asyncio.shield(work)
        finally:
            continuation.active = False
