"""A marked text echo model for checking complete and streamed routing."""
import time
import uuid
from typing import Any, AsyncIterator

from fae_plugin import Context, ModelHook, ModelRequest, ModelResponse

MARKER = "[model-plugin] "
MODEL = "plugin:echo_model/default"


def last_user_text(request: ModelRequest) -> str:
    for message in reversed(request.to_dict().get("messages", [])):
        if message.get("role") != "user":
            continue
        content = message.get("content")
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            return "\n".join(part["text"] for part in content if part.get("type") == "text")
    return ""


class EchoModel(ModelHook):
    async def match_model(self, ctx: Context, model: str) -> bool:
        return model == MODEL

    async def exec(self, ctx: Context, request: ModelRequest) -> ModelResponse:
        text = last_user_text(request)
        metadata = {"id": f"echo-{uuid.uuid4().hex}", "created": int(time.time()), "model": MODEL}
        if request.stream:
            return ModelResponse.streaming(self.chunks(metadata, text))
        return ModelResponse.completed({
            **metadata,
            "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": MARKER + text},
                         "finish_reason": "stop"}],
        })

    async def chunks(self, metadata: dict[str, Any], text: str) -> AsyncIterator[dict[str, Any]]:
        base = {**metadata, "object": "chat.completion.chunk"}
        yield {**base, "choices": [{"index": 0, "delta": {"role": "assistant", "content": MARKER},
                                   "finish_reason": None}]}
        yield {**base, "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": None}]}
        yield {**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}
