"""Onion continuations live in a single invocation and may be awaited once."""
from __future__ import annotations
import asyncio
from typing import Any, Awaitable, Callable, Generic, TypeVar
from .context import Context
from .errors import InvalidHookNext
from .hooks import ToolsHook
from .types import ModelRequest, ModelResponse, ToolRequest, ToolResponse, SessionMessage, UserMemory

Input = TypeVar("Input")
Output = TypeVar("Output")


class HookNext(Generic[Input, Output]):
    def __init__(self, callback: Callable[[Input], Awaitable[Output]],
                 check: Callable[[Any], None], owner_check: Callable[[], bool] | None = None) -> None:
        self._callback = callback
        self._check = check
        self._owner = asyncio.current_task()
        self._used = False
        self._active = True
        self._owner_check = owner_check

    async def __call__(self, value: Input) -> Output:
        if (not self._active or self._used or asyncio.current_task() is not self._owner
                or (self._owner_check is not None and not self._owner_check())):
            raise InvalidHookNext("next must be awaited once in its owning method")
        self._used = True
        self._check(value)
        result = await self._callback(value)
        return result

    def _close(self) -> None:
        self._active = False


class ChainSingleAgentHook:
    async def on_query(self, ctx: Context, query: str, *, next: HookNext[str, str]) -> str:
        return await next(query)

    async def on_prompt(self, ctx: Context, prompt: str, *, next: HookNext[str, str]) -> str:
        return await next(prompt)

    async def on_memory(self, ctx: Context, memories: list[UserMemory],
                        *, next: HookNext[list[UserMemory], list[UserMemory]]) -> list[UserMemory]:
        return await next(memories)

    async def on_history(self, ctx: Context, history: list[SessionMessage],
                         *, next: HookNext[list[SessionMessage], list[SessionMessage]]) -> list[SessionMessage]:
        return await next(history)

    async def on_compression(self, ctx: Context, request: ModelRequest, *, next: HookNext[ModelRequest, ModelRequest]) -> ModelRequest:
        return await next(request)

    async def on_model_req(self, ctx: Context, request: ModelRequest, *, next: HookNext[ModelRequest, ModelRequest]) -> ModelRequest:
        return await next(request)

    async def on_model_resp(self, ctx: Context, response: ModelResponse,
                            *, next: HookNext[ModelResponse, ModelResponse]) -> ModelResponse:
        return await next(response)

    async def on_tools_req(self, ctx: Context, request: ToolRequest, *, next: HookNext[ToolRequest, ToolRequest]) -> ToolRequest:
        return await next(request)

    async def on_tools_resp(self, ctx: Context, response: ToolResponse,
                            *, next: HookNext[ToolResponse, ToolResponse]) -> ToolResponse:
        return await next(response)

    async def on_save(self, ctx: Context, messages: list[SessionMessage],
                      *, next: HookNext[list[SessionMessage], list[SessionMessage]]) -> list[SessionMessage]:
        return await next(messages)


class ChainToolsHook:
    _channel: str
    channel = ToolsHook.channel

    async def desc(self, ctx: Context, tool_name: str,
                   *, next: HookNext[str, dict[str, Any]]) -> dict[str, Any]:
        return await next(tool_name)

    async def exec(self, ctx: Context, request: ToolRequest,
                   *, next: HookNext[ToolRequest, ToolResponse]) -> ToolResponse:
        return await next(request)


class ChainModelHook:
    async def match_model(self, ctx: Context, model: str) -> bool:
        raise NotImplementedError("ChainModelHook.match_model must be implemented")

    async def exec(self, ctx: Context, request: ModelRequest,
                   *, next: HookNext[ModelRequest, ModelResponse]) -> ModelResponse:
        return await next(request)
