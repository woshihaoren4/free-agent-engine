"""Configuration is evaluated once in the candidate inspection process."""
from __future__ import annotations
import inspect
import json
import re
from pathlib import Path
from typing import Any
from .types import API_VERSION, RegistrationMode, json_value


def normalize(raw: Any, plugin_id: str, source: Path) -> dict[str, Any]:
    if not isinstance(raw, dict):
        raise TypeError("plugin_config must return a dictionary")
    raw = json.loads(json.dumps(json_value(raw), allow_nan=False))
    if not re.fullmatch(r"[a-z][a-z0-9_]{0,63}", plugin_id):
        raise ValueError("Invalid plugin ID")
    allowed = {"id", "registration_mode", "plugin_version", "fae_plugin_api_version",
               "description", "engine", "mounts"}
    if set(raw) - allowed:
        raise ValueError("Unknown configuration fields")
    if "id" in raw and raw["id"] != plugin_id:
        raise ValueError("Plugin ID does not match configuration")
    raw.setdefault("registration_mode", "direct")
    raw.setdefault("fae_plugin_api_version", API_VERSION)
    RegistrationMode(raw["registration_mode"])
    if raw["fae_plugin_api_version"] != API_VERSION:
        raise ValueError("Incompatible FAE plugin API")
    version = raw.get("plugin_version")
    if not isinstance(version, str) or not re.fullmatch(
        r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
        r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
        r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?", version
    ):
        raise ValueError("plugin_version must be SemVer")
    if not isinstance(raw.get("description"), str) or not raw["description"].strip():
        raise ValueError("description is required")
    single = source.is_file()
    engine = source.suffix[1:] if single else raw.get("engine")
    if engine not in ("py", "js") or (single and raw.get("engine", engine) != engine):
        raise ValueError("Invalid or mismatched engine")
    raw["engine"] = engine
    if engine != "py":
        raise ValueError("JavaScript plugin engine is unavailable")
    mounts = raw.get("mounts")
    if not isinstance(mounts, list) or not mounts:
        raise ValueError("mounts must be a nonempty list")
    for mount in mounts:
        if not isinstance(mount, dict):
            raise TypeError("Mount must be an object")
        point = mount.get("point")
        if not isinstance(point, str):
            raise ValueError("Invalid mount point")
        locations = {"SingleAgentHook": "agent_id", "ToolsHook": "channel", "ModelHook": None}
        location = locations.get(point)
        allowed_mount = {"point", "implementation"} | ({location} if location else set())
        if point not in locations or set(mount) - allowed_mount:
            raise ValueError("Invalid mount point or location fields")
        if point == "ToolsHook":
            mount.setdefault("channel", f"fae_{plugin_id}")
        if location is not None and location in mount and (not isinstance(mount[location], str) or not mount[location].strip()):
            raise ValueError("Invalid mount location")
        entry = mount.get("implementation")
        if not isinstance(entry, str):
            raise TypeError("implementation must name a class")
        if single:
            class_name = entry
        else:
            file, separator, class_name = entry.partition(":")
            path = Path(file)
            if not separator or path.is_absolute() or ".." in path.parts or path.suffix != ".py":
                raise ValueError("Invalid implementation path")
            resolved = (source / path).resolve()
            if not resolved.is_relative_to(source.resolve()) or not resolved.is_file():
                raise ValueError("Implementation is outside the source or missing")
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", class_name):
            raise ValueError("implementation must name an exported class")
    for i, left in enumerate(mounts):
        location = {"SingleAgentHook": "agent_id", "ToolsHook": "channel", "ModelHook": None}[left["point"]]
        for right in mounts[i + 1:]:
            if (left["point"], left["implementation"]) != (right["point"], right["implementation"]):
                continue
            if location is None or left.get(location) == right.get(location) or (
                location == "agent_id" and (location not in left or location not in right)
            ):
                raise ValueError("Overlapping duplicate class mount")
    return raw


def inspect_config(module: Any, plugin_id: str, source: Path) -> dict[str, Any]:
    function = getattr(module, "plugin_config", None)
    if not inspect.isfunction(function) or inspect.iscoroutinefunction(function):
        raise ValueError("A synchronous plugin_config function is required")
    if inspect.signature(function).parameters:
        raise ValueError("plugin_config must have no parameters")
    return normalize(function(), plugin_id, source)
