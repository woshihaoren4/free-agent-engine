import asyncio
import json
import importlib.util
import os
import struct
import sys
import tempfile
import unittest
from pathlib import Path

from fae_plugin import (
    ContextState, ContextStateKind, ContextStackQuery, ContextChildSpec, InvalidHookNext,
    TaskType, TaskTypeKind, SingleAgentSource, HookNext,
)
from fae_plugin.config import inspect_config, normalize
from fae_plugin.registration import ModuleLoader, register
from fae_plugin.types import encode


class TypesTest(unittest.TestCase):
    def test_model_mount_has_no_location_and_requires_match_method(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "model.py"
            source.write_text("""from fae_plugin import ModelHook
class Demo(ModelHook):
    async def exec(self, ctx, request):
        raise AssertionError()
""")
            raw = {"plugin_version": "1.0.0", "description": "Model",
                   "mounts": [{"point": "ModelHook", "implementation": "Demo"}]}
            declaration = normalize(raw, "model", source)
            self.assertNotIn("model", declaration["mounts"][0])
            with self.assertRaisesRegex(ValueError, "match_model must be implemented"):
                register([{"id": "model", "source": str(source), "declaration": declaration}])
            raw["mounts"][0]["model"] = "old-model"
            with self.assertRaises(ValueError):
                normalize(raw, "model", source)

    def test_member_modules_isolate_plain_relative_and_package_imports(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for member, number in (("a", 1), ("b", 2)):
                source = root / member
                (source / "pkg").mkdir(parents=True)
                (source / "helper.py").write_text(f"VALUE = {number}\n")
                (source / "pkg" / "__init__.py").write_text("from . import nested\n")
                (source / "pkg" / "nested.py").write_text("import helper\nVALUE = helper.VALUE\n")
                (source / "plugin.py").write_text(
                    "import pkg.nested\nfrom . import helper\nVALUE = (pkg.nested.VALUE, helper.VALUE)\n")
                module = ModuleLoader(member, source).load(source / "plugin.py")
                self.assertEqual(module.VALUE, (number, number))

    def test_state_fields_null_output_and_numeric_limits(self):
        self.assertEqual(encode(ContextState.succeeded(None))["output"], {"json": None})
        with self.assertRaises(ValueError):
            ContextState(ContextStateKind.RUNNING, reason="wrong")
        with self.assertRaises(ValueError):
            ContextStackQuery(after_sequence=-1)
        with self.assertRaises(ValueError):
            ContextChildSpec(turn_id=True)
        with self.assertRaises(TypeError):
            ContextChildSpec(execution="true")
        with self.assertRaises(TypeError):
            ContextStackQuery(phase="unknown")
        with self.assertRaises(ValueError):
            TaskType(TaskTypeKind.ANY)
        with self.assertRaises(ValueError):
            SingleAgentSource("paths", config="config")

    def test_config_defaults_and_class_validation(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "demo.py"
            source.write_text("""
from fae_plugin import SingleAgentHook
count = 0
def plugin_config():
    global count
    count += 1
    return {"plugin_version":"1.0.0", "description":"Demo",
            "mounts":[{"point":"SingleAgentHook","implementation":"Demo"}]}
class Demo(SingleAgentHook):
    async def on_prompt(self, ctx, prompt):
        return prompt + "!"
""")
            module = ModuleLoader("demo", source.parent).load(source)
            config = inspect_config(module, "demo", source)
            self.assertEqual(module.count, 1)
            self.assertEqual(config["registration_mode"], "direct")
            self.assertEqual(config["fae_plugin_api_version"], "0.1.0")
            records = register([{"id": "demo", "source": str(source), "declaration": config}])
            self.assertEqual(len(records), 1)
            invalid = dict(config, registration_mode="chain")
            with self.assertRaises(ValueError):
                register([{"id": "demo", "source": str(source), "declaration": invalid}])
            invalid = dict(config, filter={})
            with self.assertRaises(ValueError):
                normalize(invalid, "demo", source)


class ContinuationTest(unittest.IsolatedAsyncioTestCase):
    async def test_next_once_and_owner_task(self):
        async def callback(value):
            return value + "!"
        next = HookNext(callback, lambda value: None)
        with self.assertRaises(InvalidHookNext):
            await asyncio.create_task(next("foreign"))
        self.assertEqual(await next("ok"), "ok!")
        with self.assertRaises(InvalidHookNext):
            await next("again")
        other = HookNext(callback, lambda value: None)
        other._close()
        with self.assertRaises(InvalidHookNext):
            await other("late")


class WorkerTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.processes = []

    async def asyncTearDown(self):
        for process in self.processes:
            process.stdin.close()
            try:
                await asyncio.wait_for(process.wait(), 3)
            except asyncio.TimeoutError:
                process.kill()
                await process.wait()
        self.directory.cleanup()

    async def start(self, sources, *, mode="direct", point="SingleAgentHook", classes=("Demo",)):
        members = []
        self.contexts = {}
        for plugin_id, code in sources:
            source = self.root / f"{plugin_id}.py"
            source.write_text(code)
            config = normalize({
                "plugin_version": "1.0.0", "description": plugin_id, "registration_mode": mode,
                "mounts": [{"point": point, "implementation": name} for name in classes],
            }, plugin_id, source)
            members.append({"id": plugin_id, "source": str(source), "declaration": config})
            for index in range(len(classes)):
                self.contexts[f"{plugin_id}:{index}"] = {"handle": plugin_id, "identity": {"agent_id": "demo"}}
        manifest = self.root / f"manifest-{len(self.processes)}.json"
        manifest.write_text(json.dumps({"members": members}))
        process = await asyncio.create_subprocess_exec(
            sys.executable, "-m", "fae_plugin._worker", str(manifest),
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE, env=os.environ.copy())
        self.processes.append(process)
        ready = await self.receive(process)
        self.assertEqual(ready.get("method"), "ready", ready)
        return process

    async def send(self, process, frame):
        data = json.dumps(frame).encode()
        process.stdin.write(struct.pack(">I", len(data)) + data)
        await process.stdin.drain()

    async def receive(self, process):
        async def read():
            size = struct.unpack(">I", await process.stdout.readexactly(4))[0]
            return json.loads(await process.stdout.readexactly(size))
        return await asyncio.wait_for(read(), 5)

    async def call(self, process, id, value, *, method="on_prompt", point="SingleAgentHook",
                   group=None, location="demo", model_members=None):
        await self.send(process, {"id": id, "method": "call", "params": {
            "point": point, "method": method, "location": location, "argument": value,
            "contexts": self.contexts, "group": group or id, "timeout_ms": 3000,
            "model_members": model_members}})

    async def test_model_match_is_strict_bool_with_creator_details(self):
        process = await self.start([("bad", """
from fae_plugin import ModelHook
class Demo(ModelHook):
    def match_model(self, ctx, model):
        assert ctx.agent_id == "demo"
        return 1
    async def exec(self, ctx, request):
        raise AssertionError("must not execute")
""")], point="ModelHook")
        await self.call(process, "select", "native", point="ModelHook", method="match_model")
        error = (await self.receive(process))["error"]
        self.assertIn("must return bool", error["message"])
        self.assertEqual(error["plugin_id"], "bad")
        self.assertEqual(error["method"], "match_model")

    async def test_chain_model_predicates_skip_layers_and_selection_is_not_repeated(self):
        process = await self.start([
            ("a", """
from fae_plugin import ChainModelHook, ModelResponse
class Demo(ChainModelHook):
    def __init__(self):
        self.matches = 0
    async def match_model(self, ctx, model):
        self.matches += 1
        return model.startswith("family-")
    async def exec(self, ctx, request, *, next):
        if request.model == "family-one":
            assert self.matches == 1, "predicate was repeated before execution"
        response = await next(request)
        response.value["choices"][0]["message"]["content"] += ":a"
        return response
"""),
            ("b", """
from fae_plugin import ChainModelHook
class Demo(ChainModelHook):
    async def match_model(self, ctx, model):
        return False
    async def exec(self, ctx, request, *, next):
        raise AssertionError("unmatched chain layer executed")
"""),
            ("c", """
from fae_plugin import ChainModelHook, ModelResponse
class Demo(ChainModelHook):
    async def match_model(self, ctx, model):
        return model == "family-one"
    async def exec(self, ctx, request, *, next):
        return ModelResponse.completed({"id":"r","object":"chat.completion","created":0,
            "model":request.model,"choices":[{"index":0,"message":{"role":"assistant","content":"c"},
            "finish_reason":"stop"}]})
""")], mode="chain", point="ModelHook")
        await self.call(process, "select", "family-one", point="ModelHook", method="match_model")
        members = (await self.receive(process))["result"]
        self.assertEqual(members, ["a:0", "c:0"])
        await self.call(process, "exec", {"model": "family-one"}, point="ModelHook", method="exec",
                        model_members=members)
        response = (await self.receive(process))["result"]
        self.assertEqual(response["response"]["choices"][0]["message"]["content"], "c:a")
        await self.call(process, "miss", "unmatched", point="ModelHook", method="match_model")
        self.assertEqual((await self.receive(process))["result"], [])
        # A matches this alias, but its terminal handler C does not.
        await self.call(process, "no-terminal", {"model": "family-two"},
                        point="ModelHook", method="exec")
        error = (await self.receive(process))["error"]
        self.assertIn("No terminal ModelHook.exec handler", error["message"])

    async def test_direct_model_classes_conflict_before_exec(self):
        process = await self.start([("models", """
from fae_plugin import ModelHook
class First(ModelHook):
    async def match_model(self, ctx, model):
        return model == "shared"
    async def exec(self, ctx, request):
        raise AssertionError("conflicting implementation executed")
class Second(First):
    pass
""")], point="ModelHook", classes=("First", "Second"))
        await self.call(process, "conflict", {"model": "shared"}, point="ModelHook", method="exec")
        error = (await self.receive(process))["error"]
        self.assertEqual(error["code"], "plugin_model_conflict")

    async def test_async_sessions_overlap_and_respond_by_id(self):
        process = await self.start([("demo", """
import asyncio
from fae_plugin import SingleAgentHook
class Demo(SingleAgentHook):
    def __init__(self):
        self.entered = asyncio.Event()
        self.release = asyncio.Event()
    async def on_prompt(self, ctx, prompt):
        if prompt == "S1":
            self.entered.set()
            await self.release.wait()
        else:
            await self.entered.wait()
            self.release.set()
        return prompt + "!"
""")])
        await self.call(process, "one", "S1")
        await self.call(process, "two", "S2")
        results = [await self.receive(process), await self.receive(process)]
        self.assertEqual({r["id"]: r["result"] for r in results}, {"one": "S1!", "two": "S2!"})

    async def test_sync_threads_overlap(self):
        process = await self.start([("demo", """
import threading
from fae_plugin import SingleAgentHook
class Demo(SingleAgentHook):
    def __init__(self):
        self.barrier = threading.Barrier(2)
    def on_prompt(self, ctx, prompt):
        self.barrier.wait(timeout=2)
        return prompt + "!"
""")])
        await self.call(process, "one", "S1")
        await self.call(process, "two", "S2")
        results = [await self.receive(process), await self.receive(process)]
        self.assertEqual({r["id"]: r["result"] for r in results}, {"one": "S1!", "two": "S2!"})

    async def test_chain_order_short_circuit_and_direct_composition(self):
        process = await self.start([
            ("a", """from fae_plugin import ChainSingleAgentHook
class Demo(ChainSingleAgentHook):
    async def on_prompt(self, ctx, prompt, *, next):
        result = await next(prompt + "[A")
        return result + "A]"
"""),
            ("b", """from fae_plugin import ChainSingleAgentHook
class Demo(ChainSingleAgentHook):
    async def on_prompt(self, ctx, prompt, *, next):
        if prompt.startswith("stop"):
            return prompt + "STOP"
        return (await next(prompt + "[B")) + "B]"
"""),
            ("c", """from fae_plugin import ChainSingleAgentHook
class Demo(ChainSingleAgentHook):
    async def on_prompt(self, ctx, prompt, *, next):
        return prompt + "C"
""")], mode="chain")
        await self.call(process, "one", "start")
        self.assertEqual((await self.receive(process))["result"], "start[A[B CB]A]".replace(" ", ""))
        await self.call(process, "two", "stop")
        self.assertEqual((await self.receive(process))["result"], "stop[ASTOPA]")

    async def test_model_is_pulled_and_usage_survives(self):
        process = await self.start([("demo", """
from fae_plugin import ModelHook, ModelResponse
class Demo(ModelHook):
    async def match_model(self, ctx, model):
        return model == "plugin:demo/default"
    async def exec(self, ctx, request):
        return ModelResponse.streaming(self.chunks())
    async def chunks(self):
        yield {"id":"demo","object":"chat.completion.chunk","model":"demo","created":0,
               "choices":[{"index":0,"delta":{"content":"first"},"finish_reason":None}]}
        yield {"id":"demo","object":"chat.completion.chunk","model":"demo","created":0,
               "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}
        yield {"id":"demo","object":"chat.completion.chunk","model":"demo","created":0,
               "choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}
""")], point="ModelHook")
        await self.call(process, "model", {"model": "plugin:demo/default", "stream": True},
                        point="ModelHook", method="exec", location="plugin:demo/default")
        response = await self.receive(process)
        stream_id = response["result"]["stream_id"]
        chunks = []
        for n in range(4):
            await self.send(process, {"id": f"next-{n}", "method": "stream.next",
                                     "params": {"stream_id": stream_id}})
            result = (await self.receive(process))["result"]
            if result["eof"]:
                break
            chunks.append(result["chunk"])
        self.assertEqual(chunks[0]["choices"][0]["delta"]["content"], "first")
        self.assertEqual(chunks[-1]["usage"]["total_tokens"], 2)
        self.assertEqual(len(chunks), 3)

    async def test_premature_stream_eof_preserves_first_chunk_and_reports_error(self):
        process = await self.start([("broken", """
from fae_plugin import ModelHook, ModelResponse
class Demo(ModelHook):
    async def match_model(self, ctx, model):
        return model == "plugin:broken/default"
    async def exec(self, ctx, request):
        return ModelResponse.streaming(self.chunks())
    async def chunks(self):
        yield {"id":"demo","object":"chat.completion.chunk","model":"demo","created":0,
               "choices":[{"index":0,"delta":{"content":"visible"},"finish_reason":None}]}
""")], point="ModelHook")
        await self.call(process, "model", {"model": "plugin:broken/default", "stream": True},
                        point="ModelHook", method="exec", location="plugin:broken/default")
        stream_id = (await self.receive(process))["result"]["stream_id"]
        for index in range(2):
            await self.send(process, {"id": f"next-{index}", "method": "stream.next",
                                     "params": {"stream_id": stream_id}})
            frame = await self.receive(process)
            if index == 0:
                self.assertEqual(frame["result"]["chunk"]["choices"][0]["delta"]["content"], "visible")
            else:
                self.assertIn("before choices finished", frame["error"]["message"])

    async def test_chain_stream_preserves_inner_outer_identity_and_error_creator(self):
        process = await self.start([
            ("a", """
from fae_plugin import ChainModelHook, ModelResponse
import sys
CURRENT_MEMBER = sys.modules["__main__"].CURRENT_MEMBER
class Demo(ChainModelHook):
    async def match_model(self, ctx, model):
        return model == "plugin:shared/default"
    async def exec(self, ctx, request, *, next):
        response = await next(request)
        async def wrap():
            async for chunk in response:
                assert CURRENT_MEMBER.get() == "a:0", CURRENT_MEMBER.get()
                chunk["choices"][0]["delta"]["content"] += ":a"
                yield chunk
        return ModelResponse.streaming(wrap())
"""),
            ("b", """
from fae_plugin import ChainModelHook, ModelResponse
import sys
CURRENT_MEMBER = sys.modules["__main__"].CURRENT_MEMBER
class Demo(ChainModelHook):
    async def match_model(self, ctx, model):
        return model == "plugin:shared/default"
    async def exec(self, ctx, request, *, next):
        async def chunks():
            assert CURRENT_MEMBER.get() == "b:0", CURRENT_MEMBER.get()
            yield {"id":"demo","object":"chat.completion.chunk","model":"demo","created":0,
                   "choices":[{"index":0,"delta":{"content":"b"},"finish_reason":None}]}
            assert CURRENT_MEMBER.get() == "b:0", CURRENT_MEMBER.get()
            raise RuntimeError("inner generator failed")
        return ModelResponse.streaming(chunks())
""")], mode="chain", point="ModelHook")
        await self.call(process, "model", {"model": "plugin:shared/default", "stream": True},
                        point="ModelHook", method="exec", location="plugin:shared/default")
        response = await self.receive(process)
        self.assertIn("result", response, response)
        stream_id = response["result"]["stream_id"]
        await self.send(process, {"id": "first", "method": "stream.next",
                                 "params": {"stream_id": stream_id}})
        first = await self.receive(process)
        self.assertIn("result", first, first)
        self.assertEqual(first["result"]["chunk"]["choices"][0]["delta"]["content"], "b:a")
        await self.send(process, {"id": "error", "method": "stream.next",
                                 "params": {"stream_id": stream_id}})
        error = (await self.receive(process))["error"]
        self.assertEqual(error["message"], "inner generator failed")
        self.assertEqual(error["plugin_id"], "b")
        self.assertEqual(error["implementation"], "Demo")
        self.assertEqual(error["method"], "exec")
        self.assertEqual(error["call_id"], "model")

    @unittest.skipUnless(importlib.util.find_spec("httpx"), "HTTP sample requires httpx")
    async def test_real_http_model_sample_streams_before_upstream_finishes_and_completes(self):
        release = asyncio.Event()

        async def serve(reader, writer):
            headers = await reader.readuntil(b"\r\n\r\n")
            length = next(int(line.split(b":", 1)[1]) for line in headers.split(b"\r\n")
                          if line.lower().startswith(b"content-length:"))
            request = json.loads(await reader.readexactly(length))
            if request.get("stream"):
                writer.write(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n"
                             b"Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
                async def event(value):
                    data = ("data: " + json.dumps(value) + "\n\n").encode()
                    writer.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
                    await writer.drain()
                base = {"id": "http", "model": "upstream", "object": "chat.completion.chunk", "created": 0}
                await event({**base, "choices": [{"index": 0, "delta": {"content": "first"},
                                                "finish_reason": None}]})
                await release.wait()
                await event({**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
                await event({**base, "choices": [], "usage": {
                    "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})
                writer.write(b"0\r\n\r\n")
            else:
                body = json.dumps({"id": "http", "object": "chat.completion", "model": "upstream",
                                   "created": 0, "choices": [{"index": 0, "finish_reason": "stop",
                                                            "message": {"role": "assistant", "content": "done"}}]}).encode()
                writer.write(f"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {len(body)}\r\n"
                             "Connection: close\r\n\r\n".encode() + body)
            await writer.drain()
            writer.close()
            await writer.wait_closed()

        server = await asyncio.start_server(serve, "127.0.0.1", 0)
        try:
            url = f"http://127.0.0.1:{server.sockets[0].getsockname()[1]}/chat"
            sample = (Path(__file__).parents[1] / "examples/http_model/plugin.py").read_text()
            sample = sample.replace("class HttpModel", "class Demo")
            sample = sample.replace("httpx.AsyncClient(timeout=", "httpx.AsyncClient(trust_env=False, timeout=")
            for key, value in (("URL", url), ("NAME", "upstream"), ("KEY", "test")):
                sample = sample.replace(f'os.environ["FAE_PLUGIN_MODEL_{key}"]', repr(value))
                sample = sample.replace(f"os.environ['FAE_PLUGIN_MODEL_{key}']", repr(value))
            process = await self.start([("http", sample)], point="ModelHook")
            await self.call(process, "stream", {"model": "plugin:http_model/default", "stream": True,
                                                "messages": []},
                            point="ModelHook", method="exec", location="plugin:http_model/default")
            stream_id = (await self.receive(process))["result"]["stream_id"]
            await self.send(process, {"id": "first", "method": "stream.next",
                                     "params": {"stream_id": stream_id}})
            first = await self.receive(process)
            self.assertIn("result", first, first)
            self.assertEqual(first["result"]["chunk"]["choices"][0]["delta"]["content"], "first")
            self.assertFalse(release.is_set())
            release.set()
            for index in range(3):
                await self.send(process, {"id": f"tail-{index}", "method": "stream.next",
                                         "params": {"stream_id": stream_id}})
                result = (await self.receive(process))["result"]
                if index == 1:
                    self.assertEqual(result["chunk"]["usage"]["total_tokens"], 2)
                if index == 2:
                    self.assertTrue(result["eof"])
            await self.call(process, "completed", {"model": "plugin:http_model/default", "stream": False,
                                                   "messages": []},
                            point="ModelHook", method="exec", location="plugin:http_model/default")
            result = (await self.receive(process))["result"]
            self.assertEqual(result["kind"], "completed")
            self.assertEqual(result["response"]["choices"][0]["message"]["content"], "done")
        finally:
            release.set()
            server.close()
            await server.wait_closed()


if __name__ == "__main__":
    unittest.main()
