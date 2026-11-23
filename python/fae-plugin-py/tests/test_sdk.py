import asyncio
import json
import importlib.util
import os
import py_compile
import struct
import sys
import tempfile
import unittest
from pathlib import Path

from fae_plugin import (
    Context, ContextData, ContextChildSpec, ExecutionStatus, InvocationStatus, InputStatus,
    InvalidHookNext, TextContent, Succeeded, HookNext,
)
from fae_plugin._protocol import request, response, validate
from fae_plugin._protocol import hook_value
from fae_plugin.config import inspect_config, normalize
from fae_plugin.registration import ModuleLoader, register
from fae_plugin.types import encode


class TypesTest(unittest.TestCase):
    def test_memory_and_history_use_registered_business_fields(self):
        from fae_plugin import MemoryView, SessionMessageRole, UserMemoryCategory
        value = MemoryView.from_dict({
            "history": [{"role": "user", "content": "hello"}], "history_offset": 0, "summary": None,
            "user_memories": [{"id": 1, "category": "preference", "content": "concise",
                               "confidence": "user_stated", "updated_at": "original-format"}],
        })
        self.assertEqual(value.history[0].role, SessionMessageRole.USER)
        self.assertEqual(value.user_memories[0].category, UserMemoryCategory.PREFERENCE)
        for method, invalid in (("on_memory", [{"id": -1}]),
                                ("on_history", [{"role": "invalid", "content": "x"}]),
                                ("on_model_req", {"model": "x"}),
                                ("on_tools_req", {"arguments": {}})):
            with self.assertRaises((ValueError, TypeError)):
                hook_value("SingleAgentHook", method, invalid)

    def test_context_data_roundtrip_keeps_nested_facts_and_no_capabilities(self):
        target = {"type": "agent", "agent_id": "demo"}
        snapshot = {
            "user_info": {"user_id": "alice", "name": None},
            "session_config": {
                "session_id": "session", "name": "chat", "lifecycle": "open",
                "created_at": "2026-11-23T01:00:00Z", "updated_at": "2026-11-23T01:00:00Z",
                "target": target, "workspace": {"root": "/tmp", "project_files": []},
                "options": {}, "channels": [{"type": "sdk", "client_id": "python"}],
            },
            "memory_view": {"history": [{"role": "user", "content": "hello"}],
                            "history_offset": 0, "summary": None, "user_memories": []},
            "latest_execution_view": {
                "execution_id": "execution", "target": target,
                "effective": {"target": target, "workspace": "/tmp", "options": {}, "definitions": []},
                "status": "succeeded", "started_at": None, "ended_at": None,
                "outcome": {"type": "succeeded", "result": [{"type": "json", "value": None}]},
                "invocations": [{
                    "invocation_id": "invocation", "parent_invocation_id": None, "kind": "plan",
                    "plan_id": "plan", "task_id": None, "runtime_id": None, "agent_id": "demo",
                    "workflow_id": None, "node_id": None, "model": {"provider": "test", "model": "demo"},
                    "status": "succeeded",
                }],
                "inputs": [{
                    "request_id": "input", "execution_id": "execution", "status": "not_applied",
                    "reason": "finished", "received_at": "2026-11-23T01:00:00Z", "processed_at": None,
                }],
            },
        }
        value = ContextData.from_dict(snapshot)
        execution = value.latest_execution_view
        self.assertEqual(execution.status, ExecutionStatus.SUCCEEDED)
        self.assertEqual(execution.invocations[0].status, InvocationStatus.SUCCEEDED)
        self.assertEqual(execution.inputs[0].status, InputStatus.NOT_APPLIED)
        encoded = encode(value)
        # Rust omits the unset allowlisted option; decoding also accepts its explicit null.
        encoded["session_config"]["options"] = {}
        encoded["latest_execution_view"]["effective"]["options"] = {}
        self.assertEqual(encoded, snapshot)
        self.assertFalse(hasattr(Context, "set_state"))
        self.assertFalse(hasattr(Context, "bind_scope"))
        with self.assertRaises(ValueError):
            ContextData.from_dict({**snapshot, "handle": "forged"})

    def test_member_modules_load_current_source_despite_stale_bytecode(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "plugin.py"
            helper = root / "helper.py"
            source.write_text("import helper\nVALUE = 'old' + helper.VALUE\n")
            helper.write_text("VALUE = 'old'\n")
            for file in (source, helper):
                before = file.stat()
                py_compile.compile(str(file), doraise=True)
                file.write_text(file.read_text().replace("'old'", "'new'"))
                os.utime(file, ns=(before.st_atime_ns, before.st_mtime_ns))
            module = ModuleLoader("fresh", root).load(source)
            self.assertEqual(module.VALUE, "newnew")

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

    def test_public_content_and_restricted_child_spec(self):
        self.assertEqual(encode(Succeeded([TextContent("result")])),
                         {"type": "succeeded", "result": [{"type": "text", "text": "result"}]})
        with self.assertRaises(ValueError):
            ContextChildSpec("")
        with self.assertRaises(TypeError):
            ContextChildSpec(operation="child", workspace="/tmp")
        with self.assertRaises(TypeError):
            encode(object())
        with self.assertRaises(ValueError):
            validate({"id": "old", "method": "call", "params": {}})
        with self.assertRaises(ValueError):
            ContextData.from_dict({"user_info": {"user_id": "forged"}})

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
            self.assertEqual(config["fae_plugin_api_version"], "0.2.0")
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
        self.next_requests = {}

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
                self.contexts[f"{plugin_id}:{index}"] = {
                    "binding": {"handle": f"{plugin_id}:{index}", "execution_id": None, "invocation_id": None},
                    "session_id": None, "summary": None}
        manifest = self.root / f"manifest-{len(self.processes)}.json"
        manifest.write_text(json.dumps({"members": members}))
        process = await asyncio.create_subprocess_exec(
            sys.executable, "-m", "fae_plugin._worker", str(manifest),
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE, env=os.environ.copy())
        self.processes.append(process)
        ready = await self.receive(process)
        self.assertEqual(ready["path"], "/request/plugin/worker/hello", ready)
        self.assertEqual(ready["payload"]["api_version"], "0.2.0")
        await self.send(process, response(ready, {}, msg="ready"))
        return process

    async def send(self, process, frame):
        data = json.dumps(frame).encode()
        process.stdin.write(struct.pack(">I", len(data)) + data)
        await process.stdin.drain()

    async def receive(self, process):
        async def read():
            while True:
                size = struct.unpack(">I", await process.stdout.readexactly(4))[0]
                frame = validate(json.loads(await process.stdout.readexactly(size)))
                if frame["path"] == "/request/plugin/next":
                    identifier = f"next-dispatch-{len(self.next_requests)}-{frame['request_id']}"
                    self.next_requests[identifier] = frame
                    await self.send(process, request(identifier, frame["path"], frame["payload"],
                                                     frame.get("session_id")))
                elif frame["request_id"] in self.next_requests:
                    original = self.next_requests.pop(frame["request_id"])
                    await self.send(process, response(original, frame["payload"]["data"],
                                                     code=frame["payload"]["code"], msg=frame["payload"]["msg"]))
                else:
                    return frame
        return await asyncio.wait_for(read(), 5)

    async def call(self, process, id, value, *, method="on_prompt", point="SingleAgentHook",
                   group=None, location="demo", model_members=None):
        if point == "ModelHook" and method == "exec":
            value = {"messages": [], **value}
        await self.send(process, request(id, "/request/plugin/invoke", {
            "point": point, "method": method, "location": location, "argument": value,
            "contexts": self.contexts, "group": group or id, "timeout_ms": 3000,
            "model_members": model_members}))

    async def data(self, process):
        frame = await self.receive(process)
        self.assertEqual(frame["payload"]["code"], 0, frame)
        return frame["payload"]["data"]

    async def error(self, process):
        frame = await self.receive(process)
        self.assertNotEqual(frame["payload"]["code"], 0, frame)
        self.assertIsNone(frame["payload"]["data"])
        return frame["payload"]

    async def next_stream(self, process, id, stream_id, call_id="model"):
        await self.send(process, request(id, "/request/plugin/stream/next",
            {"call_id": call_id, "stream_id": stream_id, "access": None}))

    async def test_model_match_is_strict_bool_with_creator_details(self):
        process = await self.start([("bad", """
from fae_plugin import ModelHook
class Demo(ModelHook):
    def match_model(self, ctx, model):
        return 1
    async def exec(self, ctx, request):
        raise AssertionError("must not execute")
""")], point="ModelHook")
        await self.call(process, "select", "native", point="ModelHook", method="match_model")
        error = await self.error(process)
        self.assertIn("must return bool", error["msg"])

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
        members = await self.data(process)
        self.assertEqual(members, ["a:0", "c:0"])
        await self.call(process, "exec", {"model": "family-one"}, point="ModelHook", method="exec",
                        model_members=members)
        response = await self.data(process)
        self.assertEqual(response["response"]["choices"][0]["message"]["content"], "c:a")
        await self.call(process, "miss", "unmatched", point="ModelHook", method="match_model")
        self.assertEqual(await self.data(process), [])
        # A matches this alias, but its terminal handler C does not.
        await self.call(process, "no-terminal", {"model": "family-two"},
                        point="ModelHook", method="exec")
        error = await self.error(process)
        self.assertIn("No terminal ModelHook.exec handler", error["msg"])

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
        error = await self.error(process)
        self.assertEqual(error["code"], 409)

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
        self.assertEqual({r["request_id"]: r["payload"]["data"] for r in results}, {"one": "S1!", "two": "S2!"})

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
        self.assertEqual({r["request_id"]: r["payload"]["data"] for r in results}, {"one": "S1!", "two": "S2!"})

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
        self.assertEqual(await self.data(process), "start[A[B CB]A]".replace(" ", ""))
        await self.call(process, "two", "stop")
        self.assertEqual(await self.data(process), "stop[ASTOPA]")

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
        stream_id = (await self.data(process))["stream_id"]
        chunks = []
        for n in range(4):
            await self.next_stream(process, f"next-{n}", stream_id)
            result = await self.data(process)
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
        stream_id = (await self.data(process))["stream_id"]
        for index in range(2):
            await self.next_stream(process, f"next-{index}", stream_id)
            frame = await self.receive(process)
            if index == 0:
                self.assertEqual(frame["payload"]["data"]["chunk"]["choices"][0]["delta"]["content"], "visible")
            else:
                self.assertIn("before choices finished", frame["payload"]["msg"])

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
        stream_id = (await self.data(process))["stream_id"]
        await self.next_stream(process, "first", stream_id)
        first = await self.data(process)
        self.assertEqual(first["chunk"]["choices"][0]["delta"]["content"], "b:a")
        await self.next_stream(process, "error", stream_id)
        error = await self.error(process)
        self.assertEqual(error["msg"], "inner generator failed")

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
            stream_id = (await self.data(process))["stream_id"]
            await self.next_stream(process, "first", stream_id, "stream")
            first = await self.data(process)
            self.assertEqual(first["chunk"]["choices"][0]["delta"]["content"], "first")
            self.assertFalse(release.is_set())
            release.set()
            for index in range(3):
                await self.next_stream(process, f"tail-{index}", stream_id, "stream")
                result = await self.data(process)
                if index == 1:
                    self.assertEqual(result["chunk"]["usage"]["total_tokens"], 2)
                if index == 2:
                    self.assertTrue(result["eof"])
            await self.call(process, "completed", {"model": "plugin:http_model/default", "stream": False,
                                                   "messages": []},
                            point="ModelHook", method="exec", location="plugin:http_model/default")
            result = await self.data(process)
            self.assertEqual(result["kind"], "completed")
            self.assertEqual(result["response"]["choices"][0]["message"]["content"], "done")
        finally:
            release.set()
            server.close()
            await server.wait_closed()


if __name__ == "__main__":
    unittest.main()
