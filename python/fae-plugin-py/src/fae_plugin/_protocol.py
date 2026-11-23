"""Four-byte big-endian length followed by UTF-8 JSON."""
import asyncio
import json
import struct
from typing import Any, BinaryIO

MAX_FRAME = 4 * 1024 * 1024
PROTOCOL_VERSION = 4

PLUGIN_PATHS = {
    "worker/hello", "inspect", "invoke", "next", "context/get", "context/export",
    "context/fork", "context/cancel", "context/stacks", "output/send", "stream/next", "stream/close",
    "call/release", "host/get_engine", "host/get_session",
}
SESSION_PATHS = {"execution/steer", "execution/stop", "execution/get"}


def _object(value: Any, required: set[str], optional: set[str] = frozenset()) -> None:
    if not isinstance(value, dict) or required - set(value) or set(value) - required - optional:
        raise ValueError("Payload fields do not match registered schema")


def _access(value: Any) -> None:
    _object(value, {"call_id", "member", "binding"})
    _object(value["binding"], {"handle"}, {"execution_id", "invocation_id"})
    if not all(isinstance(value[key], str) and value[key] for key in ("call_id", "member")):
        raise ValueError("Invalid capability owner")
    binding = value["binding"]
    if not isinstance(binding["handle"], str) or not binding["handle"]:
        raise ValueError("Invalid capability handle")
    if (binding.get("execution_id") is None) != (binding.get("invocation_id") is None):
        raise ValueError("Incomplete execution binding")
    for key in ("execution_id", "invocation_id"):
        identity = binding.get(key)
        if identity is not None and (not isinstance(identity, str) or not identity or len(identity.encode()) > 256):
            raise ValueError("Invalid execution identity")

def hook_value(point: str, method: str, value: Any, *, output: bool = False) -> None:
    from .data import SessionMessage, UserMemory
    methods = {"SingleAgentHook": {"on_query", "on_prompt", "on_memory", "on_history", "on_compression",
                "on_model_req", "on_model_resp", "on_tools_req", "on_tools_resp", "on_save"},
               "ToolsHook": {"desc", "exec"}, "ModelHook": {"match_model", "exec"}}
    if method not in methods.get(point, set()):
        raise ValueError("Unregistered hook method")
    if method in {"on_query", "on_prompt"} or (method in {"match_model", "desc"} and not output):
        if not isinstance(value, str):
            raise ValueError("Hook requires text")
    elif method == "match_model":
        if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
            raise ValueError("Model selection requires member ids")
    elif method in {"on_memory", "on_history", "on_save"}:
        if not isinstance(value, list):
            raise ValueError("Hook requires an array")
        cls = UserMemory if method == "on_memory" else SessionMessage
        for item in value:
            cls.from_dict(item)
    elif method == "on_model_resp" or (point == "ModelHook" and output):
        if not isinstance(value, dict):
            raise ValueError("Model result requires an object")
        if value.get("kind") == "completed":
            _object(value, {"kind", "response"})
            completion = value["response"]
            if (not isinstance(completion, dict) or not {"id", "object", "created", "model", "choices"} <= set(completion)
                    or not isinstance(completion["choices"], list)):
                raise ValueError("Invalid model completion")
        elif value.get("kind") in ({"streaming", "host_stream"} if output else {"streaming"}):
            _object(value, {"kind", "stream_id"})
            if not isinstance(value["stream_id"], str) or not value["stream_id"]:
                raise ValueError("Invalid stream reference")
        else:
            raise ValueError("Invalid model response variant")
    elif method == "on_tools_resp" or (point == "ToolsHook" and method == "exec" and output):
        if not isinstance(value, dict) or set(value) not in ({"result"}, {"reference"}, {"result", "reference"}):
            raise ValueError("Invalid tool result")
        if "reference" in value:
            if not isinstance(value["reference"], str) or not value["reference"]:
                raise ValueError("Invalid tool reference")
            if value.get("result") is not None and not isinstance(value["result"], str):
                raise ValueError("Invalid tool input")
        elif not isinstance(value["result"], str):
            raise ValueError("Tool result requires text")
    elif point == "ToolsHook" and method == "desc":
        if not isinstance(value, dict):
            raise ValueError("Tool description requires an object")
    elif method == "on_tools_req" or point == "ToolsHook":
        _object(value, {"tool_name", "arguments"})
        if not all(isinstance(value[k], str) for k in ("tool_name", "arguments")):
            raise ValueError("Invalid tool request")
    else:
        if (not isinstance(value, dict) or not isinstance(value.get("model"), str)
                or not isinstance(value.get("messages"), list)):
            raise ValueError("Invalid native model request")


def _payload(suffix: str, payload: dict[str, Any]) -> None:
    schemas = {
        "plugin/worker/hello": ({"api_version", "instances"}, set()),
        "plugin/inspect": ({"id", "source"}, set()),
        "plugin/invoke": ({"point", "method", "contexts", "argument", "group", "timeout_ms"}, {"location", "model_members"}),
        "plugin/next": ({"access", "point", "method", "argument"}, set()),
        "plugin/context/stacks": ({"access", "query"}, set()),
        "plugin/context/fork": ({"access", "spec"}, set()),
        "plugin/context/cancel": ({"access"}, {"reason"}),
        "plugin/output/send": ({"access", "content"}, set()),
        "plugin/stream/next": ({"call_id", "stream_id"}, {"access"}),
        "plugin/stream/close": ({"call_id", "stream_id"}, {"access"}),
        "plugin/call/release": ({"call_id"}, set()),
        "session/execution/steer": ({"execution_id", "input"}, set()),
        "session/execution/stop": ({"execution_id"}, {"reason"}),
        "session/execution/get": (set(), {"execution_id", "input_request_id"}),
    }
    if suffix in {"plugin/context/get", "plugin/context/export", "plugin/host/get_engine", "plugin/host/get_session"}:
        _access(payload)
        return
    required, optional = schemas[suffix]
    _object(payload, required, optional)
    if payload.get("access") is not None:
        _access(payload["access"])
    if suffix == "plugin/context/fork":
        _object(payload["spec"], {"operation"})
    if suffix == "plugin/context/stacks":
        from .data import ContextStackQuery
        query = ContextStackQuery.from_dict(payload["query"])
        if query.limit is not None and not 0 <= query.limit <= 256:
            raise ValueError("Invalid stack limit")
    if suffix == "plugin/next":
        hook_value(payload["point"], payload["method"], payload["argument"])
    if suffix == "plugin/invoke":
        methods = {"SingleAgentHook": {"on_query", "on_prompt", "on_memory", "on_history", "on_compression",
                    "on_model_req", "on_model_resp", "on_tools_req", "on_tools_resp", "on_save"},
                   "ToolsHook": {"desc", "exec"}, "ModelHook": {"match_model", "exec"}}
        if payload["method"] not in methods.get(payload["point"], set()):
            raise ValueError("Unregistered hook method")
        if type(payload["timeout_ms"]) is not int or not 0 < payload["timeout_ms"] <= 120000:
            raise ValueError("Invalid call deadline")
        if not isinstance(payload["contexts"], dict) or not payload["contexts"]:
            raise ValueError("Call requires capabilities")
        for member, reference in payload["contexts"].items():
            _object(reference, {"binding"}, {"summary", "session_id"})
            _access({"call_id": "validation", "member": member, "binding": reference["binding"]})
        hook_value(payload["point"], payload["method"], payload["argument"])


def validate(frame: dict[str, Any]) -> dict[str, Any]:
    if frame.get("version") != PROTOCOL_VERSION:
        raise ValueError("Only protocol v4 is supported")
    path = frame.get("path", "")
    if not isinstance(path, str) or not path.startswith(("/request/", "/response/")):
        raise ValueError("Invalid plugin protocol path")
    suffix = path.split("/", 2)[2]
    if suffix not in {f"plugin/{p}" for p in PLUGIN_PATHS} | {f"session/{p}" for p in SESSION_PATHS}:
        raise ValueError("Unregistered plugin protocol path")
    if set(frame) - {"version", "request_id", "session_id", "path", "payload"}:
        raise ValueError("Unknown envelope fields")
    identifier = frame.get("request_id")
    if not isinstance(identifier, str) or not identifier or len(identifier.encode()) > 256 or identifier.strip() != identifier or any(ord(c) < 32 or ord(c) == 127 for c in identifier):
        raise ValueError("Invalid request_id")
    sid = frame.get("session_id")
    if sid is not None and (not isinstance(sid, str) or not sid or len(sid.encode()) > 256 or sid.strip() != sid):
        raise ValueError("Invalid session_id")
    if suffix.startswith("session/") and sid is None:
        raise ValueError("Session operation requires session_id")
    if not isinstance(frame.get("payload"), dict) and not path.startswith("/response/"):
        raise ValueError("Request payload must be an object")
    if path.startswith("/request/"):
        _payload(suffix, frame["payload"])
        if suffix in {"plugin/worker/hello", "plugin/inspect"} and sid is not None:
            raise ValueError("Initialization cannot bind a session")
        if suffix == "plugin/invoke":
            for reference in frame["payload"]["contexts"].values():
                if reference.get("session_id") != sid or (reference["binding"].get("execution_id") is None) != (sid is None):
                    raise ValueError("Invocation session binding mismatch")
    if path.startswith("/response/"):
        body = frame["payload"]
        if not isinstance(body, dict) or set(body) != {"code", "msg", "data"}:
            raise ValueError("Invalid response payload")
        if type(body["code"]) is not int or body["code"] not in {0, 400, 403, 404, 409, 500, 503, 507}:
            raise ValueError("Invalid response code")
        if not isinstance(body["msg"], str) or (body["code"] != 0 and body["data"] is not None):
            raise ValueError("Invalid error response")
    return frame


def request(request_id: str, path: str, payload: dict[str, Any], session_id: str | None = None) -> dict[str, Any]:
    frame = {"version": PROTOCOL_VERSION, "request_id": request_id, "path": path, "payload": payload}
    if session_id is not None:
        frame["session_id"] = session_id
    return validate(frame)


def response(message: dict[str, Any], data: Any = None, *, code: int = 0, msg: str = "ok") -> dict[str, Any]:
    if code == 0 and message["path"] in {"/request/plugin/invoke", "/request/plugin/next"}:
        hook_value(message["payload"]["point"], message["payload"]["method"], data, output=True)
    frame = {"version": PROTOCOL_VERSION, "request_id": message["request_id"],
             "path": message["path"].replace("/request/", "/response/", 1),
             "payload": {"code": code, "msg": msg, "data": data if code == 0 else None}}
    if message.get("session_id") is not None:
        frame["session_id"] = message["session_id"]
    return validate(frame)


async def reader() -> asyncio.StreamReader:
    import sys
    stream = asyncio.StreamReader(limit=MAX_FRAME + 4)
    await asyncio.get_running_loop().connect_read_pipe(
        lambda: asyncio.StreamReaderProtocol(stream), sys.stdin.buffer)
    return stream


async def read_frame(stream: asyncio.StreamReader) -> dict[str, Any]:
    size = struct.unpack(">I", await stream.readexactly(4))[0]
    if size == 0 or size > MAX_FRAME:
        raise ValueError("Invalid plugin protocol frame length")
    data = json.loads(await stream.readexactly(size), parse_constant=reject_constant)
    if not isinstance(data, dict):
        raise ValueError("Protocol frame must be an object")
    return validate(data)


def reject_constant(value: str) -> Any:
    raise ValueError(f"Invalid JSON constant {value}")


class Writer:
    def __init__(self, output: BinaryIO):
        self.output = output
        self.control: asyncio.Queue[bytes] = asyncio.Queue(64)
        self.business: asyncio.Queue[bytes] = asyncio.Queue(64)
        self.ready = asyncio.Event()

    async def send(self, frame: dict[str, Any], *, control: bool = False) -> None:
        validate(frame)
        data = json.dumps(frame, allow_nan=False, separators=(",", ":")).encode("utf-8")
        if not data or len(data) > MAX_FRAME:
            raise ValueError("Plugin protocol message exceeds 4 MiB")
        await (self.control if control else self.business).put(struct.pack(">I", len(data)) + data)
        self.ready.set()

    def _write(self, data: bytes) -> None:
        self.output.write(data)
        self.output.flush()

    async def run(self) -> None:
        while True:
            await self.ready.wait()
            queue = self.control if not self.control.empty() else self.business
            if queue.empty():
                self.ready.clear()
                continue
            data = queue.get_nowait()
            await asyncio.to_thread(self._write, data)
            if self.control.empty() and self.business.empty():
                self.ready.clear()
