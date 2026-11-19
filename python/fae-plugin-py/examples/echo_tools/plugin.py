from typing import Any
from fae_plugin import Context, ToolsHook, ToolRequest, ToolResponse


class Echo(ToolsHook):
    async def desc(self, ctx: Context, tool_name: str) -> dict[str, Any]:
        if tool_name != f"{self.channel()}__echo":
            raise ValueError(f"Unknown tool: {tool_name}")
        return {
            "name": tool_name,
            "description": "返回输入文本",
            "parameters": {
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
            },
        }

    async def exec(self, ctx: Context, request: ToolRequest) -> ToolResponse:
        if request.tool_name != f"{self.channel()}__echo":
            raise ValueError(f"Unknown tool: {request.tool_name}")
        arguments = request.arguments_json()
        if not isinstance(arguments, dict):
            raise ValueError("arguments must be an object")
        text = arguments.get("text")
        if not isinstance(text, str):
            raise ValueError("text must be a string")
        return ToolResponse.completed(text)
