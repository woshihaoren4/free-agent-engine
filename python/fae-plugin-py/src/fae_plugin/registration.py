"""Construct immutable member records; chain members are not Rust execution nodes."""
from __future__ import annotations
import builtins
import importlib.util
import inspect
import sys
import types
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from .chain import ChainModelHook, ChainSingleAgentHook, ChainToolsHook
from .hooks import ModelHook, SingleAgentHook, ToolsHook

AGENT_METHODS = ("on_query", "on_prompt", "on_memory", "on_history", "on_compression",
                 "on_model_req", "on_model_resp", "on_tools_req", "on_tools_resp", "on_save")


class ModuleLoader:
    """Resolve local imports in a per-member namespace, including plain import helper."""
    def __init__(self, plugin_id: str, root: Path):
        self.root = root.resolve()
        self.namespace = f"_fae_plugin_{plugin_id}"
        package = types.ModuleType(self.namespace)
        package.__path__ = [str(self.root)]
        sys.modules[self.namespace] = package
        self.modules: dict[Path, Any] = {}

    def load(self, file: Path) -> Any:
        file = file.resolve()
        if file in self.modules:
            return self.modules[file]
        if not file.is_relative_to(self.root):
            raise ValueError("Code entry escapes the plugin root")
        relative = file.relative_to(self.root).with_suffix("")
        is_package = file.name == "__init__.py"
        pieces = relative.parts[:-1] if is_package else relative.parts
        for n in range(1, len(pieces)):
            name = self.namespace + "." + ".".join(pieces[:n])
            if name not in sys.modules:
                init = self.root.joinpath(*pieces[:n], "__init__.py")
                if init.is_file():
                    self.load(init)
                    continue
                package = types.ModuleType(name)
                package.__path__ = [str(self.root.joinpath(*pieces[:n]))]
                sys.modules[name] = package
        name = self.namespace + ("." + ".".join(pieces) if pieces else "")
        spec = importlib.util.spec_from_file_location(
            name, file, submodule_search_locations=[str(file.parent)] if is_package else None)
        if spec is None or spec.loader is None:
            raise ValueError("Cannot load implementation module")
        module = importlib.util.module_from_spec(spec)
        module.__dict__["__builtins__"] = {**vars(builtins), "__import__": self._import}
        self.modules[file] = module
        sys.modules[name] = module
        parent, _, child = name.rpartition(".")
        if parent in sys.modules:
            setattr(sys.modules[parent], child, module)
        spec.loader.exec_module(module)
        return module

    def _import(self, name: str, globals: Any = None, locals: Any = None,
                fromlist: Any = (), level: int = 0) -> Any:
        local_name = name
        if level:
            resolved = importlib.util.resolve_name("." * level + name, globals["__package__"])
            if resolved == self.namespace:
                local_name = ""
            elif resolved.startswith(self.namespace + "."):
                local_name = resolved[len(self.namespace) + 1:]
            else:
                return builtins.__import__(name, globals, locals, fromlist, level)
        path = self.root.joinpath(*local_name.split(".")) if local_name else self.root
        file, package = path.with_suffix(".py"), path / "__init__.py"
        module: Any = None
        if local_name and file.is_file():
            module = self.load(file)
        elif package.is_file():
            module = self.load(package)
        elif path.is_dir():
            key = self.namespace + ("." + local_name if local_name else "")
            if key not in sys.modules:
                module = types.ModuleType(key)
                module.__path__ = [str(path)]
                sys.modules[key] = module
            module = sys.modules[key]
        if module is not None:
            if fromlist:
                for item in fromlist:
                    if item != "*" and not hasattr(module, item):
                        child_file, child_package = path / f"{item}.py", path / item / "__init__.py"
                        if child_file.is_file() or child_package.is_file():
                            setattr(module, item, self.load(child_file if child_file.is_file() else child_package))
                return module
            return sys.modules[self.namespace + "." + local_name.split(".")[0]]
        return builtins.__import__(name, globals, locals, fromlist, level)


@dataclass(frozen=True)
class Instance:
    key: str
    plugin_id: str
    mount: dict[str, Any]
    object: Any
    base: type
    mode: str

    def matches(self, point: str, location: str | None) -> bool:
        if self.mount["point"] != point:
            return False
        if point == "ModelHook":
            return True
        field = {"SingleAgentHook": "agent_id", "ToolsHook": "channel"}[point]
        return self.mount.get(field) == location or (field == "agent_id" and field not in self.mount)


def register(members: list[dict[str, Any]]) -> list[Instance]:
    records = []
    for member in members:
        source = Path(member["source"])
        loader = ModuleLoader(member["id"], source.parent if source.is_file() else source)
        config = member["declaration"]
        mode = config["registration_mode"]
        bases = {"direct": {"SingleAgentHook": SingleAgentHook, "ToolsHook": ToolsHook, "ModelHook": ModelHook},
                 "chain": {"SingleAgentHook": ChainSingleAgentHook, "ToolsHook": ChainToolsHook,
                           "ModelHook": ChainModelHook}}[mode]
        for index, mount in enumerate(config["mounts"]):
            if source.is_file():
                module, class_name = loader.load(source), mount["implementation"]
            else:
                file, class_name = mount["implementation"].split(":")
                module = loader.load(source / file)
            cls = getattr(module, class_name, None)
            base = bases[mount["point"]]
            if not inspect.isclass(cls) or not issubclass(cls, base) or cls is base:
                raise ValueError(f"{member['id']}:{class_name} must inherit {base.__name__}")
            methods = AGENT_METHODS if mount["point"] == "SingleAgentHook" else (
                ("desc", "exec") if mount["point"] == "ToolsHook" else ("match_model", "exec"))
            for method in methods:
                function = getattr(cls, method)
                if mode == "chain" and not inspect.iscoroutinefunction(function):
                    raise ValueError("Chain hook methods must be async")
                signature = inspect.signature(function)
                params = list(signature.parameters.values())
                chained = mode == "chain" and method != "match_model"
                count = 4 if chained else 3
                if len(params) != count or any(p.kind not in (
                    inspect.Parameter.POSITIONAL_ONLY, inspect.Parameter.POSITIONAL_OR_KEYWORD)
                    for p in params[:3]):
                    raise ValueError(f"Invalid {class_name}.{method} signature")
                if chained and (params[3].name != "next" or params[3].kind != inspect.Parameter.KEYWORD_ONLY
                                        or params[3].default != inspect.Parameter.empty):
                    raise ValueError("Chain next must be a required keyword-only argument")
                if (method == "match_model" or (mode == "direct" and mount["point"] != "SingleAgentHook")) and function is getattr(base, method):
                    raise ValueError(f"{class_name}.{method} must be implemented")
            obj: Any = cls()
            if not isinstance(obj, cls):
                if inspect.iscoroutine(obj):
                    obj.close()
                raise ValueError(f"{member['id']}:{class_name} did not construct a hook instance")
            if mount["point"] == "ToolsHook":
                setattr(obj, "_channel", mount["channel"])
            records.append(Instance(f"{member['id']}:{index}", member["id"], mount, obj, base, mode))
    for record in records:
        if record.mode != "chain" or record.mount["point"] == "SingleAgentHook":
            continue
        point = record.mount["point"]
        location = record.mount["channel"] if point == "ToolsHook" else None
        for method in ("desc", "exec") if point == "ToolsHook" else ("exec",):
            if not any(r.matches(point, location) and getattr(type(r.object), method) is not getattr(r.base, method)
                       for r in records):
                raise ValueError(f"Chain {point} {location} has no {method} implementation")
    return records
