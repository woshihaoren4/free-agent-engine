import asyncio
import tempfile
import unittest
from pathlib import Path

from fae_plugin import PlanNext, TaskMetadata, TaskRequest, TaskResponse
from fae_plugin._providers import ProviderRegistry
from fae_plugin.registration import register


SOURCE = '''
from fae_plugin import ChainPlan, ChainRuntime, Runtime, PlanNext, TaskResponse
import asyncio

class Audit(ChainPlan):
    def id(self): return "audit_plan"
    async def init(self):
        self.saved = self.next_plan
        return await self.next_plan.init()

class Report(ChainPlan):
    def id(self): return "report_plan"
    async def init(self):
        self.count = 0
        self.env["mutated"] = True
        return PlanNext.Tasks([])
    async def next(self, task_result):
        self.count += 1
        return PlanNext.End()

class Echo(Runtime):
    def __init__(self): self.count = 0
    def id(self): return "echo_runtime"
    def tys(self): return ["echo"]
    async def exec(self, task):
        original = self.ctx
        await asyncio.sleep(0)
        assert self.ctx is original and task.ctx is original
        return TaskResponse(task.meta, task.req, task.ctx)
    async def select(self, ty, cond):
        self.count += 1
        return self.count

class Outer(ChainRuntime):
    def id(self): return "chain_runtime"
    def tys(self): return ["echo"]
    async def exec(self, task):
        result = await self.next_runtime.exec(task)
        try:
            await self.next_runtime.exec(task)
        except RuntimeError:
            pass
        else:
            raise AssertionError("Continuation reused")
        return result

class Inner(ChainRuntime):
    def id(self): return "chain_runtime"
    def tys(self): return ["echo"]
    async def exec(self, task):
        return TaskResponse(task.meta, task.req, task.ctx)
'''


class ProvidersTest(unittest.IsolatedAsyncioTestCase):
    def records(self, mounts, mode="chain"):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        source = Path(directory.name) / "plugin.py"
        source.write_text(SOURCE)
        return register([{"id": "example", "source": str(source), "declaration": {
            "registration_mode": mode, "mounts": mounts}}])

    async def test_plan_groups_are_independent_and_continuation_expires(self):
        registry = ProviderRegistry(self.records([
            {"point": "Plan", "id": "audit_plan", "implementation": "Audit", "next_plan_id": "report_plan"},
            {"point": "Plan", "id": "report_plan", "implementation": "Report"},
        ]))
        env = {"source": "weekly"}
        registry.create("audit_plan", "one", env)
        registry.create("report_plan", "two", env)
        await registry.plan("audit_plan", "one", "init", (), ctx=object())
        self.assertEqual(env, {"source": "weekly"})
        self.assertEqual(registry.groups["one"].objects[0].env, env)
        self.assertEqual(registry.groups["two"].objects[0].env, env)
        with self.assertRaises(RuntimeError):
            await registry.groups["one"].objects[0].saved.init()
        await registry.release("audit_plan", "one")
        await registry.release("audit_plan", "one")
        self.assertIn("two", registry.groups)
        with self.assertRaises(ValueError):
            await registry.release("report_plan", "one")

    async def test_runtime_context_is_local_and_payload_is_unchanged(self):
        registry = ProviderRegistry(self.records([{
            "point": "Runtime", "id": "echo_runtime", "implementation": "Echo", "task_types": ["echo"],
        }], "direct"))
        values = [None, True, 12, "plain", [1, None], {"req": {"env": "preserved"}}]
        async def execute(index, value):
            ctx = object()
            task = TaskRequest(TaskMetadata(f"t{index}", {"Any": "echo"}), value, ctx)
            return await registry.runtime("echo_runtime", "exec", (task,), ctx=ctx)
        self.assertEqual(await asyncio.gather(*(execute(i, value) for i, value in enumerate(values))), values)
        with self.assertRaises(RuntimeError):
            _ = registry.runtimes["echo_runtime"][0].object.ctx

    async def test_runtime_chain_is_one_call(self):
        registry = ProviderRegistry(self.records([
            {"point": "Runtime", "id": "chain_runtime", "implementation": "Outer", "task_types": ["echo"]},
            {"point": "Runtime", "id": "chain_runtime", "implementation": "Inner", "task_types": ["echo"]},
        ]))
        ctx = object()
        task = TaskRequest(TaskMetadata("one", {"Any": "echo"}), {"data": True}, ctx)
        self.assertEqual(await registry.runtime("chain_runtime", "exec", (task,), ctx=ctx), task.req)

    async def test_runtime_generations_isolate_state_and_exit(self):
        registry = ProviderRegistry(self.records([{
            "point": "Runtime", "id": "echo_runtime", "implementation": "Echo", "task_types": ["echo"],
        }], "direct"))
        async def select(generation):
            return await registry.runtime("echo_runtime", "select", ({"Any": "echo"}, None),
                                          ctx=object(), generation=generation)
        self.assertEqual(await select("old"), 1)
        self.assertEqual(await select("old"), 2)
        self.assertEqual(await select("new"), 1)
        await registry.runtime("echo_runtime", "exit", (), ctx=None, generation="old")
        self.assertEqual(await select("new"), 2)
        with self.assertRaisesRegex(RuntimeError, "retired"):
            await select("old")
        self.assertNotIn(("old", "echo_runtime"), registry.runtime_objects)

    async def test_registration_rejects_duplicate_and_cyclic_plans(self):
        for mounts in [
            [{"point": "Plan", "id": "report_plan", "implementation": "Report"}] * 2,
            [{"point": "Plan", "id": "report_plan", "implementation": "Report", "next_plan_id": "report_plan"}],
        ]:
            with self.assertRaises(ValueError):
                self.records(mounts)

    def test_task_dto_and_plan_next_preserve_json_shapes(self):
        for value in [None, False, 3, "plain", [None], {"payload": "business"}]:
            task = TaskRequest(TaskMetadata("one", "Model"), value)
            self.assertEqual(TaskRequest.from_dict(task.to_dict()).req, value)
            response = TaskResponse(task.meta, value)
            self.assertEqual(TaskResponse.from_dict(response.to_dict()).resp, value)
        self.assertEqual(PlanNext.End().to_dict(), {"kind": "end"})
        self.assertEqual(PlanNext.Tasks([]).to_dict(), {"kind": "tasks", "tasks": []})
        with self.assertRaises(ValueError):
            TaskMetadata("one", {"Any": "Model"})


if __name__ == "__main__":
    unittest.main()
