from typing import Any


class Engine:
    __slots__ = ("_engine_id", "_host")

    def __init__(self, *, _host: Any, _engine_id: str):
        self._host = _host
        self._engine_id = _engine_id

    @property
    def engine_id(self) -> str:
        return self._engine_id

    def same_engine(self, other: "Engine") -> bool:
        return isinstance(other, Engine) and self._engine_id == other._engine_id
