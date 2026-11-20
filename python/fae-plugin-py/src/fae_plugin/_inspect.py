"""Candidate-only helper. stdout is reserved for one framed configuration result."""
import sys
import json
import struct
from pathlib import Path
from .config import inspect_config, normalize
from .registration import ModuleLoader


def main() -> None:
    output = sys.stdout.buffer
    sys.stdout = sys.stderr
    source = Path(sys.argv[2])
    try:
        if source.is_file():
            loader = ModuleLoader(sys.argv[1], source.parent)
            config = inspect_config(loader.load(source), sys.argv[1], source)
        else:
            config = normalize(json.loads((source / "plugin.config").read_text(encoding="utf-8")),
                               sys.argv[1], source)
        result = {"result": config}
    except Exception as error:
        result = {"error": {"code": "plugin_config_invalid", "message": str(error)}}
    data = json.dumps(result, allow_nan=False).encode()
    output.write(struct.pack(">I", len(data)) + data)
    output.flush()


if __name__ == "__main__":
    main()
