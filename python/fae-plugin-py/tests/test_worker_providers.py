import asyncio
import json
import os
import struct
import sys
import tempfile
import unittest
from pathlib import Path

from fae_plugin._protocol import response, validate, validate_worker
from fae_plugin.config import normalize
from fae_plugin.tasks import TaskMetadata

SOURCE = '''
from fae_plugin import ChainPlan, ChainRuntime, PlanNext, TaskResponse
import asyncio
class Report(ChainPlan):
    def id(self): return "report_plan"
    async def init(self):
        await self.ctx.complete(self.env)
        return PlanNext.End()
    async def next(self, task_result): return PlanNext.End()
class Echo(ChainRuntime):
    def id(self): return "echo_runtime"
    def tys(self): return ["echo"]
    async def exec(self, task):
        await asyncio.sleep(0)
        return TaskResponse(task.meta, task.req, task.ctx)
'''


class ProviderWorkerTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        root = Path(self.directory.name)
        source = root / "example.py"
        source.write_text(SOURCE)
        declaration = normalize({
            "id": "example", "fae_plugin_api_version": "0.3.0", "plugin_version": "1.0.0",
            "description": "test", "registration_mode": "chain",
            "mounts": [
                {"point": "Plan", "id": "report_plan", "implementation": "Report"},
                {"point": "Runtime", "id": "echo_runtime", "implementation": "Echo", "task_types": ["echo"]},
            ],
        }, "example", source)
        manifest = root / "manifest.json"
        manifest.write_text(json.dumps({"members": [{"id": "example", "source": str(source), "declaration": declaration}]}))
        self.process = await asyncio.create_subprocess_exec(
            sys.executable, "-m", "fae_plugin._worker", str(manifest),
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            env=os.environ.copy())
        hello = await self.receive()
        await self.send(response(hello, {}, msg="ready"))
        self.serial = 0

    async def asyncTearDown(self):
        self.process.stdin.close()
        try:
            await asyncio.wait_for(self.process.wait(), 3)
        except asyncio.TimeoutError:
            self.process.kill()
            await self.process.wait()
        stderr = await self.process.stderr.read()
        self.directory.cleanup()
        self.assertEqual(self.process.returncode, 0, stderr.decode())

    async def send(self, frame):
        validate_worker(frame)
        data = json.dumps(frame, allow_nan=False).encode()
        self.process.stdin.write(struct.pack(">I", len(data)) + data)
        await self.process.stdin.drain()

    async def receive(self):
        async def read():
            size = struct.unpack(">I", await self.process.stdout.readexactly(4))[0]
            return validate_worker(json.loads(await self.process.stdout.readexactly(size)))
        return await asyncio.wait_for(read(), 5)

    def frame(self, operation, payload, binding, *, plan=None, runtime=None):
        self.serial += 1
        identifier = f"rpc_{self.serial}"
        member = "example:0" if plan else "example:1"
        frame = {"api_version": "0.1.0", "request_id": identifier,
                 "event": f"/request/plugin/{operation}", "payload": payload,
                 "session": None, "plan": {"plan_id": plan} if plan else None,
                 "binding": {"access": {"call_id": identifier, "member": member,
                     "binding": {"handle": identifier, "execution_id": None, "invocation_id": None}}, **binding}}
        if runtime:
            frame["call"] = {"runtime_id": runtime}
        return validate_worker(frame)

    async def test_plan_instance_environment_and_complete_are_raw_json(self):
        for index, value in enumerate([None, True, "plain", [1, None], {"env": {"payload": "business"}}]):
            instance = f"instance_{index}"
            create = self.frame("plan/create", value,
                {"generation": "generation_1", "context": {"context_id": f"ctx_{index}"}, "instance_id": instance},
                plan="report_plan")
            with self.assertRaises(ValueError):
                validate(create)
            await self.send(create)
            created = await self.receive()
            self.assertEqual(created["payload"], instance)
            init = self.frame("plan/init", None, {"instance_id": instance}, plan="report_plan")
            await self.send(init)
            complete = await self.receive()
            self.assertEqual(complete["event"], "/request/plugin/context/complete")
            self.assertEqual(complete["payload"], value)
            await self.send(response(complete, None))
            finished = await self.receive()
            self.assertEqual(finished["response_id"], init["request_id"])
            self.assertEqual(finished["payload"], {"kind": "end"})
            await self.send(self.frame("plan/release", None, {"instance_id": instance}, plan="report_plan"))
            self.assertIsNone((await self.receive())["payload"])

    async def test_concurrent_runtime_exec_returns_unchanged_json_by_id(self):
        expected = {}
        for index, value in enumerate([None, False, 3, "plain", [1, None], {"req": "business"}]):
            frame = self.frame("runtime/exec", value, {
                "generation": "generation_1", "context": {"context_id": f"ctx_{index}"},
                "meta": TaskMetadata(f"task_{index}", {"Any": "echo"}, executor="echo_runtime").to_dict(),
            }, runtime="echo_runtime")
            expected[frame["request_id"]] = value
            await self.send(frame)
        results = [await self.receive() for _ in expected]
        self.assertEqual({f["response_id"]: f["payload"] for f in results}, expected)
        self.assertTrue(all(f["code"] == 0 for f in results))

    async def test_release_without_create_reply_is_idempotent_and_blocks_late_create(self):
        instance = {"instance_id": "abandoned"}
        create_binding = {**instance, "generation": "generation_1", "context": {"context_id": "ctx"}}

        async def send_plan(operation, payload, binding):
            await self.send(self.frame(f"plan/{operation}", payload, binding, plan="report_plan"))
            return await self.receive()

        # A preemptive release can arrive when the outcome of create is unknown.
        self.assertEqual((await send_plan("release", None, instance))["code"], 0)
        self.assertNotEqual((await send_plan("create", {}, create_binding))["code"], 0)
        self.assertEqual((await send_plan("release", None, instance))["code"], 0)

        # Also release an instance whose create succeeded but was abandoned.
        instance = {"instance_id": "created"}
        create_binding.update(instance)
        self.assertEqual((await send_plan("create", {}, create_binding))["code"], 0)
        self.assertEqual((await send_plan("release", None, instance))["code"], 0)
        self.assertEqual((await send_plan("release", None, instance))["code"], 0)
        self.assertNotEqual((await send_plan("init", None, instance))["code"], 0)
