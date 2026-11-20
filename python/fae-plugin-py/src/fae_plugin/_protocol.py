"""Four-byte big-endian length followed by UTF-8 JSON."""
import asyncio
import json
import struct
from typing import Any, BinaryIO

MAX_FRAME = 4 * 1024 * 1024
PROTOCOL_VERSION = 1


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
    data = json.loads(await stream.readexactly(size))
    if not isinstance(data, dict):
        raise ValueError("Protocol frame must be an object")
    return data


class Writer:
    def __init__(self, output: BinaryIO):
        self.output = output
        self.control: asyncio.Queue[bytes] = asyncio.Queue(64)
        self.business: asyncio.Queue[bytes] = asyncio.Queue(64)
        self.ready = asyncio.Event()

    async def send(self, frame: dict[str, Any], *, control: bool = False) -> None:
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
