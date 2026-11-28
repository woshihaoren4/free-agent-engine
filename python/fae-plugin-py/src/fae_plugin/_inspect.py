"""Candidate-only helper using the shared 0.1.0 inspect envelope."""
import sys
import json
import struct
from pathlib import Path
from ._protocol import MAX_FRAME, validate, response, reject_constant
from .config import inspect_config, normalize
from .registration import ModuleLoader


def main() -> None:
    output = sys.stdout.buffer
    sys.stdout = sys.stderr
    size = struct.unpack(">I", sys.stdin.buffer.read(4))[0]
    if not 0 < size <= MAX_FRAME:
        raise ValueError("Invalid inspect frame length")
    message = validate(json.loads(sys.stdin.buffer.read(size), parse_constant=reject_constant))
    if message["event"] != "/request/plugin/inspect" or message["session"] is not None:
        raise ValueError("Expected an execution-free inspect request")
    payload = message["payload"]
    if set(payload) != {"id", "source"} or payload["id"] != sys.argv[1] or payload["source"] != sys.argv[2]:
        raise ValueError("Inspect source mismatch")
    source = Path(payload["source"])
    try:
        if source.is_file():
            loader = ModuleLoader(payload["id"], source.parent)
            config = inspect_config(loader.load(source), payload["id"], source)
        else:
            config = normalize(json.loads((source / "plugin.config").read_text(encoding="utf-8")),
                               payload["id"], source)
        result = response(message, config)
    except Exception as error:
        result = response(message, code=400, msg=str(error))
    data = json.dumps(result, allow_nan=False).encode()
    if len(data) > MAX_FRAME:
        raise ValueError("Inspect response exceeds frame limit")
    output.write(struct.pack(">I", len(data)) + data)
    output.flush()


if __name__ == "__main__":
    main()
