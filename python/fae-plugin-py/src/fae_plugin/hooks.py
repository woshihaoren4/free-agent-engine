"""Direct hook defaults preserve their inputs."""
from typing import Any
from .context import Context
from .types import ModelRequest, ModelResponse, ToolRequest, ToolResponse, SessionMessage, UserMemory


class SingleAgentHook:
    async def on_query(self, ctx: Context, query: str) -> str:
        return query

    async def on_prompt(self, ctx: Context, prompt: str) -> str:
        return prompt

    async def on_memory(self, ctx: Context, memories: list[UserMemory]) -> list[UserMemory]:
        return memories

    async def on_history(self, ctx: Context, history: list[SessionMessage]) -> list[SessionMessage]:
        return history

    async def on_compression(self, ctx: Context, request: ModelRequest) -> ModelRequest:
        return request

    async def on_model_req(self, ctx: Context, request: ModelRequest) -> ModelRequest:
        return request

    async def on_model_resp(self, ctx: Context, response: ModelResponse) -> ModelResponse:
        return response

    async def on_tools_req(self, ctx: Context, request: ToolRequest) -> ToolRequest:
        return request

    async def on_tools_resp(self, ctx: Context, response: ToolResponse) -> ToolResponse:
        return response

    async def on_save(self, ctx: Context, messages: list[SessionMessage]) -> list[SessionMessage]:
        return messages


class ToolsHook:
    _channel: str
    def channel(self) -> str:
        return self._channel

    async def desc(self, ctx: Context, tool_name: str) -> dict[str, Any]:
        raise NotImplementedError("ToolsHook.desc must be implemented")

    async def exec(self, ctx: Context, request: ToolRequest) -> ToolResponse:
        raise NotImplementedError("ToolsHook.exec must be implemented")


class ModelHook:
    async def match_model(self, ctx: Context, model: str) -> bool:
        raise NotImplementedError("ModelHook.match_model must be implemented")

    async def exec(self, ctx: Context, request: ModelRequest) -> ModelResponse:
        raise NotImplementedError("ModelHook.exec must be implemented")
