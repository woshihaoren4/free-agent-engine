"""Install httpx and set FAE_PLUGIN_MODEL_URL / KEY / NAME before starting FAE."""
import json
import os
from typing import AsyncIterator, Any

import httpx
from fae_plugin import Context, ModelHook, ModelRequest, ModelResponse


class HttpModel(ModelHook):
    async def match_model(self, ctx: Context, model: str) -> bool:
        return model == "plugin:http_model/default"

    def __init__(self) -> None:
        self.url = os.environ["FAE_PLUGIN_MODEL_URL"]
        self.model = os.environ["FAE_PLUGIN_MODEL_NAME"]
        self.headers = {"Authorization": f"Bearer {os.environ['FAE_PLUGIN_MODEL_KEY']}"}

    async def exec(self, ctx: Context, request: ModelRequest) -> ModelResponse:
        payload = request.to_dict()
        payload["model"] = self.model
        if request.stream:
            return ModelResponse.streaming(self.chunks(payload))
        async with httpx.AsyncClient(timeout=httpx.Timeout(60, connect=10)) as client:
            response = await client.post(self.url, headers=self.headers, json=payload)
            response.raise_for_status()
            return ModelResponse.completed(response.json())

    async def chunks(self, payload: dict[str, Any]) -> AsyncIterator[dict[str, Any]]:
        async with httpx.AsyncClient(timeout=httpx.Timeout(60, connect=10)) as client:
            async with client.stream("POST", self.url, headers=self.headers, json=payload) as response:
                response.raise_for_status()
                data: list[str] = []
                async for line in response.aiter_lines():
                    if line.startswith("data:"):
                        data.append(line[5:].lstrip())
                    elif not line and data:
                        event = "\n".join(data)
                        data.clear()
                        if event == "[DONE]":
                            return
                        yield json.loads(event)
                if data:
                    event = "\n".join(data)
                    if event != "[DONE]":
                        yield json.loads(event)
