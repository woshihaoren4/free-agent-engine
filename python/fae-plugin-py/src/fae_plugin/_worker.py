"""Persistent worker with independent protocol, business, and stream tasks."""
from __future__ import annotations
import asyncio
import contextvars
import itertools
import json
import sys
import traceback
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from ._host import BoundHost, Host
from ._protocol import PROTOCOL_VERSION, Writer, read_frame, reader
from .chain import HookNext
from .context import Context
from .errors import HookCancelled, MissingChainHandler, PluginHostError
from .registration import register
from .types import ModelRequest, ModelResponse, NativeObject, ToolRequest, ToolResponse, SessionMessage, UserMemory, encode, json_value

CURRENT_MEMBER: contextvars.ContextVar[str] = contextvars.ContextVar("fae_plugin_member")


@dataclass
class Invocation:
    id: str
    params: dict[str, Any]
    deadline: float
    group: str
    terminal: bool = False
    released: bool = False
    task: asyncio.Task[Any] | None = None
    streams: set[str] = field(default_factory=set)
    responses: list[ModelResponse] = field(default_factory=list)

    def remaining(self) -> float:
        return max(0.0, self.deadline - asyncio.get_running_loop().time())

    def check(self) -> None:
        if self.terminal or self.released:
            raise HookCancelled("Plugin invocation is no longer active", call_id=self.id)
        if self.remaining() <= 0:
            raise PluginHostError("Plugin invocation timed out", call_id=self.id)


class HostStream:
    def __init__(self, host: BoundHost, stream_id: str):
        self.host, self.stream_id = host, stream_id
        self.closed = False
        self.busy = False

    def __aiter__(self) -> "HostStream":
        return self

    async def __anext__(self) -> dict[str, Any]:
        if self.closed:
            raise StopAsyncIteration
        if self.busy:
            raise ValueError("A stream permits only one next in flight")
        self.busy = True
        try:
            result = await self.host.call("stream.next", {"stream_id": self.stream_id})
            if result["eof"]:
                self.closed = True
                raise StopAsyncIteration
            return result["chunk"]
        finally:
            self.busy = False

    async def aclose(self) -> None:
        if not self.closed:
            self.closed = True
            await self.host.call("stream.close", {"stream_id": self.stream_id})


class OwnedIterator:
    """Keep creator identity while lazy inner/outer generators are iterated."""
    def __init__(self, iterator: Any, invocation: Invocation, record: Any, method: str):
        self.iterator = iterator.__aiter__()
        self.invocation, self.record, self.method = invocation, record, method

    def __aiter__(self) -> "OwnedIterator":
        return self

    async def __anext__(self) -> Any:
        token = CURRENT_MEMBER.set(self.record.key)
        try:
            self.invocation.check()
            chunk = await self.iterator.__anext__()
            self.invocation.check()
            return chunk
        except StopAsyncIteration:
            raise
        except BaseException as error:
            if not getattr(error, "details", {}).get("plugin_id"):
                setattr(error, "details", {**getattr(error, "details", {}),
                    "plugin_id": self.record.plugin_id,
                    "implementation": self.record.mount["implementation"],
                    "method": self.method, "call_id": self.invocation.id})
            raise
        finally:
            CURRENT_MEMBER.reset(token)

    async def aclose(self) -> None:
        token = CURRENT_MEMBER.set(self.record.key)
        try:
            close = getattr(self.iterator, "aclose", None)
            if close is not None:
                await close()
        finally:
            CURRENT_MEMBER.reset(token)


@dataclass
class Stream:
    invocation: Invocation
    iterator: Any
    member: str
    busy: bool = False
    choices: dict[int, bool] = field(default_factory=dict)
    tools: dict[tuple[int, int], dict[str, str]] = field(default_factory=dict)
    first: bool = True
    task: asyncio.Task[Any] | None = None


def check_value(point: str, method: str, value: Any, *, output: bool) -> None:
    expected: Any
    if method in ("on_query", "on_prompt") or (point == "ToolsHook" and method == "desc" and not output):
        expected = str
    elif method in ("on_memory", "on_history", "on_save"):
        expected = list
    elif method == "on_model_resp" or (point == "ModelHook" and output):
        expected = ModelResponse
    elif method == "on_tools_resp" or (point == "ToolsHook" and method == "exec" and output):
        expected = ToolResponse
    elif point == "ToolsHook" and method == "desc":
        expected = dict
    else:
        expected = (NativeObject, dict)
    if not isinstance(value, expected):
        raise TypeError(f"{point}.{method} {'output' if output else 'input'} has invalid type")
    if isinstance(value, list) and not all(isinstance(item, (NativeObject, dict)) for item in value):
        raise TypeError(f"{method} requires native objects")
    if isinstance(value, ToolResponse) and value._reference is None and not isinstance(value.result, str):
        raise TypeError("Tool response must contain a string or a host reference")
    if not isinstance(value, (ModelResponse, ToolResponse)):
        json_value(encode(value))


class Worker:
    def __init__(self, writer: Writer, members: list[dict[str, Any]]):
        self.writer, self.host = writer, Host(writer)
        self.records = register(members)
        self.calls: dict[str, Invocation] = {}
        self.streams: dict[str, Stream] = {}
        self.host_streams: dict[str, Invocation] = {}
        self.stream_ids = itertools.count()
        self.queue: dict[str, deque[Invocation]] = {}
        self.round_robin: deque[str] = deque()
        self.active: dict[str, int] = {}
        self.wake = asyncio.Event()
        self.executor = ThreadPoolExecutor(max_workers=4, thread_name_prefix="fae-plugin")
        self.thread_slots = asyncio.Semaphore(4)
        self.controls: set[asyncio.Task[Any]] = set()
        self.admitted = 0

    def spawn(self, coroutine: Any) -> None:
        task = asyncio.create_task(coroutine)
        self.controls.add(task)
        task.add_done_callback(self.controls.discard)

    async def result(self, id: str, *, result: Any = None, error: BaseException | None = None,
                     control: bool = False) -> None:
        if error is None:
            frame = {"id": id, "result": result}
        else:
            frame = {"id": id, "error": {"code": getattr(error, "code", "plugin_call_failed"),
                    "message": str(error), "call_id": id,
                    **getattr(error, "details", {})}}
        await self.writer.send(frame, control=control)

    async def enqueue(self, frame: dict[str, Any]) -> None:
        params = frame["params"]
        group = params["group"]  # The host supplies a validated user/session or root-run group.
        pending = sum(len(q) for q in self.queue.values())
        if pending >= 64 or len(self.queue.get(group, ())) >= 16 or len(self.calls) >= 96:
            error = PluginHostError("Plugin pending call queue is full", resource="calls")
            error.code = "plugin_busy"
            await self.result(frame["id"], error=error)
            return
        if frame["id"] in self.calls:
            await self.result(frame["id"], error=ValueError("Duplicate call ID"))
            return
        invocation = Invocation(frame["id"], params,
                                asyncio.get_running_loop().time() + params["timeout_ms"] / 1000, group)
        self.calls[invocation.id] = invocation
        if group not in self.queue:
            self.queue[group] = deque()
            self.round_robin.append(group)
        self.queue[group].append(invocation)
        self.wake.set()

    async def schedule(self) -> None:
        while True:
            await self.wake.wait()
            self.wake.clear()
            count = len(self.round_robin)
            for _ in range(count):
                group = self.round_robin.popleft()
                queue = self.queue[group]
                while queue and (queue[0].released or queue[0].remaining() <= 0):
                    invocation = queue.popleft()
                    if not invocation.released:
                        await self.result(invocation.id, error=PluginHostError("Queue deadline exceeded"))
                        await self.release(invocation)
                if queue and self.admitted < 8 and self.active.get(group, 0) < 4:
                    invocation = queue.popleft()
                    self.admitted += 1
                    self.active[group] = self.active.get(group, 0) + 1
                    invocation.task = asyncio.create_task(self.execute(invocation))
                    self.wake.set()
                if queue:
                    self.round_robin.append(group)
                else:
                    del self.queue[group]
            # Queue deadlines are observed even when every business slot is occupied.
            if self.queue:
                earliest = min(inv.remaining() for q in self.queue.values() for inv in q)
                asyncio.get_running_loop().call_later(earliest, self.wake.set)

    def decode(self, invocation: Invocation, host: BoundHost) -> Any:
        point, method, value = (invocation.params[k] for k in ("point", "method", "argument"))
        if method == "on_model_resp":
            if value["kind"] == "completed":
                return ModelResponse.completed(value["response"])
            return ModelResponse.streaming(HostStream(host, value["stream_id"]))
        if method == "on_tools_resp":
            return ToolResponse(value.get("result"), _reference=value.get("reference"))
        if method in ("on_tools_req",) or (point == "ToolsHook" and method == "exec"):
            return ToolRequest(value)
        if method in ("on_model_req", "on_compression") or point == "ModelHook":
            return ModelRequest(value)
        if isinstance(value, list):
            cls = UserMemory if method == "on_memory" else SessionMessage
            return [cls(v) for v in value]
        return value

    async def method(self, function: Any, ctx: Context, value: Any, **kwargs: Any) -> Any:
        import inspect
        if inspect.iscoroutinefunction(function):
            return await function(ctx, value, **kwargs)
        await self.thread_slots.acquire()
        copied = contextvars.copy_context()
        future = asyncio.get_running_loop().run_in_executor(
            self.executor, lambda: copied.run(function, ctx, value, **kwargs))
        # Cancelling the waiter cannot stop a Python thread. Keep its actual slot until it exits.
        future.add_done_callback(lambda _: self.thread_slots.release())
        return await asyncio.shield(future)

    async def matching_models(self, invocation: Invocation, records: list[Any], model: str) -> list[Any]:
        if not isinstance(model, str):
            raise TypeError("match_model requires a model string")
        matching = []
        for record in records:
            host = BoundHost(self.host, invocation, record.key)
            ctx = Context(_host=host, _data=invocation.params["contexts"][record.key])
            token = CURRENT_MEMBER.set(record.key)
            try:
                invocation.check()
                result = await self.method(record.object.match_model, ctx, model)
                invocation.check()
                if type(result) is not bool:
                    raise TypeError("match_model must return bool")
                if result:
                    matching.append(record)
            except BaseException as error:
                setattr(error, "details", {**getattr(error, "details", {}),
                    "plugin_id": record.plugin_id,
                    "implementation": record.mount["implementation"],
                    "method": "match_model", "call_id": invocation.id})
                raise
            finally:
                CURRENT_MEMBER.reset(token)
        if len(matching) > 1 and matching[0].mode == "direct":
            conflict = PluginHostError(f"Multiple model implementations match {model}")
            conflict.code = "plugin_model_conflict"
            raise conflict
        return matching

    async def execute(self, invocation: Invocation) -> None:
        try:
            point, method = invocation.params["point"], invocation.params["method"]
            matching = [r for r in self.records if r.matches(
                point, invocation.params.get("location"))]
            if point == "ModelHook":
                if method == "match_model":
                    models = await asyncio.wait_for(
                        self.matching_models(invocation, matching, invocation.params["argument"]),
                        invocation.remaining())
                    await self.result(invocation.id, result=[r.key for r in models])
                    return
                selected = invocation.params.get("model_members")
                if selected is None:
                    matching = await asyncio.wait_for(
                        self.matching_models(invocation, matching, invocation.params["argument"]["model"]),
                        invocation.remaining())
                else:
                    if (not isinstance(selected, list) or not all(isinstance(key, str) for key in selected)
                            or len(set(selected)) != len(selected)
                            or any(key not in {r.key for r in matching} for key in selected)):
                        raise ValueError("Invalid selected model members")
                    matching = [r for r in matching if r.key in selected]
            if not matching:
                raise MissingChainHandler("No matching plugin implementation")
            contexts = invocation.params["contexts"]
            first_host = BoundHost(self.host, invocation, matching[0].key)
            value = self.decode(invocation, first_host)
            if isinstance(value, ModelResponse) and value.is_streaming:
                invocation.responses.append(value)

            async def layer(index: int, value: Any) -> Any:
                invocation.check()
                check_value(point, method, value, output=False)
                if index == len(matching):
                    if point != "SingleAgentHook":
                        raise MissingChainHandler(f"No terminal {point}.{method} handler")
                    return value
                record = matching[index]
                host = BoundHost(self.host, invocation, record.key)
                ctx = Context(_host=host, _data=contexts[record.key])
                token = CURRENT_MEMBER.set(record.key)
                continuation: HookNext[Any, Any] | None = None
                try:
                    if record.mode == "chain":
                        def validate_next(v: Any) -> None:
                            invocation.check()
                            check_value(point, method, v, output=False)
                        continuation = HookNext(lambda next_value: layer(index + 1, next_value),
                                                validate_next)
                        result = await self.method(getattr(record.object, method), ctx, value, next=continuation)
                    else:
                        result = await self.method(getattr(record.object, method), ctx, value)
                    invocation.check()
                    check_value(point, method, result, output=True)
                    if isinstance(result, ModelResponse) and result.is_streaming:
                        if not hasattr(result, "_member"):
                            setattr(result, "_member", record.key)
                            if not isinstance(result.value, HostStream):
                                result.value = OwnedIterator(result.value, invocation, record, method)
                        if not any(r is result for r in invocation.responses):
                            if len(invocation.responses) >= 64:
                                close = getattr(result.value, "aclose", None)
                                if close is not None:
                                    await close()
                                error = PluginHostError("Model stream reference limit exceeded",
                                                        resource="stream_references")
                                error.code = "plugin_busy"
                                raise error
                            invocation.responses.append(result)
                    if record.mode == "direct" and index + 1 < len(matching):
                        result = await layer(index + 1, result)
                    return result
                except BaseException as error:
                    if not getattr(error, "details", {}).get("plugin_id"):
                        setattr(error, "details", {**getattr(error, "details", {}),
                            "plugin_id": record.plugin_id,
                            "implementation": record.mount["implementation"],
                            "method": method, "call_id": invocation.id})
                    raise
                finally:
                    if continuation:
                        continuation._close()
                    CURRENT_MEMBER.reset(token)

            output = await asyncio.wait_for(layer(0, value), invocation.remaining())
            invocation.check()
            result: Any
            if isinstance(output, ModelResponse):
                if output.is_streaming:
                    group_streams = sum(s.invocation.group == invocation.group for s in self.streams.values())
                    group_streams += sum(i.group == invocation.group for i in self.host_streams.values())
                    if len(self.streams) + len(self.host_streams) >= 16 or group_streams >= 4:
                        close = getattr(output.value, "aclose", None)
                        if close is not None:
                            await close()
                        error = PluginHostError("Active model stream limit exceeded", resource="streams")
                        error.code = "plugin_busy"
                        raise error
                    if output is value and isinstance(output.value, HostStream):
                        self.host_streams[invocation.id] = invocation
                        result = {"kind": "host_stream", "stream_id": output.value.stream_id}
                    else:
                        stream_id = f"stream-{next(self.stream_ids)}"
                        self.streams[stream_id] = Stream(invocation, output.__aiter__(),
                                                        getattr(output, "_member", matching[0].key))
                        invocation.streams.add(stream_id)
                        result = {"kind": "streaming", "stream_id": stream_id}
                else:
                    result = {"kind": "completed", "response": output.value}
            elif isinstance(output, ToolResponse):
                result = {"reference": output._reference} if output._reference else {"result": output.result}
            else:
                result = encode(output)
            await self.result(invocation.id, result=result)
        except asyncio.CancelledError:
            await self.result(invocation.id, error=HookCancelled("Plugin call cancelled"), control=True)
        except BaseException as error:
            print(traceback.format_exc(), file=sys.stderr)
            await self.result(invocation.id, error=error)
            await self.release(invocation)
        finally:
            self.admitted -= 1
            self.active[invocation.group] -= 1
            if self.active[invocation.group] == 0:
                self.active.pop(invocation.group)
            self.wake.set()
            # Host sends release after it has decoded the result and acquired any stream references.

    async def next_stream(self, frame: dict[str, Any]) -> None:
        stream_id = frame["params"]["stream_id"]
        stream = self.streams.get(stream_id)
        if stream is None or stream.busy:
            await self.result(frame["id"], error=ValueError("Invalid or busy stream"))
            return
        stream.busy = True
        stream.task = asyncio.current_task()
        token = CURRENT_MEMBER.set(stream.member)
        try:
            stream.invocation.check()
            chunk = await asyncio.wait_for(stream.iterator.__anext__(),
                                           min(30 if stream.first else 60, stream.invocation.remaining()))
            stream.first = False
            chunk = json_value(encode(chunk))
            if len(json.dumps(chunk).encode()) > 64 * 1024:
                raise ValueError("Model chunk exceeds 64 KiB")
            if not isinstance(chunk, dict) or not isinstance(chunk.get("choices"), list):
                raise ValueError("Invalid OpenAI stream chunk")
            if (not isinstance(chunk.get("id"), str) or not isinstance(chunk.get("model"), str)
                    or type(chunk.get("created")) is not int
                    or chunk.get("object") != "chat.completion.chunk"):
                raise ValueError("Invalid OpenAI stream metadata")
            for choice in chunk["choices"]:
                index = choice["index"]
                if type(index) is not int or index < 0:
                    raise ValueError("Invalid model choice index")
                if stream.choices.get(index):
                    raise ValueError("Delta after choice finish")
                delta = choice.get("delta")
                if not isinstance(delta, dict):
                    raise ValueError("Invalid model delta")
                for tool in delta.get("tool_calls") or []:
                    tool_index = tool.get("index")
                    if type(tool_index) is not int or tool_index < 0:
                        raise ValueError("Invalid tool delta index")
                    fields = stream.tools.setdefault((index, tool_index),
                                                     {"id": "", "name": "", "arguments": ""})
                    function = tool.get("function") or {}
                    for key, fragment in (("id", tool.get("id")), ("name", function.get("name")),
                                          ("arguments", function.get("arguments"))):
                        if fragment is not None:
                            if not isinstance(fragment, str):
                                raise ValueError("Invalid tool call fragment")
                            fields[key] += fragment
                    if sum(len(v.encode()) for v in fields.values()) > 1024 * 1024:
                        raise ValueError("Tool call arguments exceed 1 MiB")
                if choice.get("finish_reason") is not None:
                    for (choice_index, _), fields in stream.tools.items():
                        if choice_index == index:
                            if not fields["id"] or not fields["name"]:
                                raise ValueError("Incomplete tool call")
                            json.loads(fields["arguments"])
                stream.choices[index] = choice.get("finish_reason") is not None
            await self.result(frame["id"], result={"eof": False, "chunk": chunk})
        except StopAsyncIteration:
            if not stream.choices or not all(stream.choices.values()):
                await self.result(frame["id"], error=ValueError("Model stream ended before choices finished"))
            else:
                await self.result(frame["id"], result={"eof": True})
            await self.close_stream(stream_id)
        except BaseException as error:
            await self.result(frame["id"], error=error)
            await self.close_stream(stream_id)
        finally:
            stream.busy = False
            stream.task = None
            CURRENT_MEMBER.reset(token)

    async def close_stream(self, stream_id: str) -> None:
        stream = self.streams.pop(stream_id, None)
        if stream:
            stream.invocation.streams.discard(stream_id)
            if stream.task and stream.task is not asyncio.current_task() and not stream.task.done():
                stream.task.cancel()
                await asyncio.gather(stream.task, return_exceptions=True)
            try:
                if hasattr(stream.iterator, "aclose"):
                    await stream.iterator.aclose()
            except Exception:
                pass

    async def release(self, invocation: Invocation) -> None:
        if invocation.released:
            return
        invocation.released = True
        self.host_streams.pop(invocation.id, None)
        queue = self.queue.get(invocation.group)
        if queue is not None:
            try:
                queue.remove(invocation)
            except ValueError:
                pass
        self.wake.set()
        if invocation.task and invocation.task is not asyncio.current_task() and not invocation.task.done():
            invocation.task.cancel()
        for stream_id in list(invocation.streams):
            await self.close_stream(stream_id)
        for response in invocation.responses:
            close = getattr(response.value, "aclose", None)
            if close is not None:
                try:
                    await close()
                except Exception:
                    pass
        invocation.responses.clear()
        self.calls.pop(invocation.id, None)

    async def run(self) -> None:
        self.spawn(self.schedule())
        await self.writer.send({"method": "ready", "params": {
            "protocol_version": PROTOCOL_VERSION, "api_version": "0.1.0",
            "instances": [r.key for r in self.records]}}, control=True)
        stream = await reader()
        try:
            while True:
                frame = await read_frame(stream)
                method = frame.get("method")
                if method is None:
                    self.host.response(frame)
                elif method == "call":
                    await self.enqueue(frame)
                elif method == "stream.next":
                    if len(self.controls) >= 64:
                        await self.result(frame["id"], error=PluginHostError("Control capacity exceeded"))
                    else:
                        self.spawn(self.next_stream(frame))
                elif method in ("release", "cancel"):
                    invocation = self.calls.get(frame["params"]["call_id"])
                    if invocation:
                        invocation.terminal = method == "cancel"
                        self.spawn(self.release(invocation))
                elif method == "stream.close":
                    self.spawn(self.close_stream(frame["params"]["stream_id"]))
                else:
                    raise ValueError("Unknown host protocol method")
        finally:
            self.host.close()
            for invocation in list(self.calls.values()):
                await self.release(invocation)
            for task in self.controls:
                task.cancel()
            self.executor.shutdown(wait=False, cancel_futures=True)


async def main() -> None:
    output = sys.stdout.buffer
    sys.stdout = sys.stderr
    writer = Writer(output)
    writer_task = asyncio.create_task(writer.run())
    try:
        worker = Worker(writer, json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))["members"])
        await worker.run()
    except BaseException as error:
        await writer.send({"method": "startup_error", "params": {"message": str(error)}}, control=True)
        # Drain the startup error before closing stdout.
        while not writer.control.empty():
            await asyncio.sleep(0)
        raise
    finally:
        writer_task.cancel()


if __name__ == "__main__":
    asyncio.run(main())
