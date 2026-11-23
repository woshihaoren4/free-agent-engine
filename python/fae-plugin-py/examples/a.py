from fae_plugin import ChainSingleAgentHook, Context, HookNext


def plugin_config() -> dict:
    return {
        "registration_mode": "chain",
        "plugin_version": "1.0.0",
        "fae_plugin_api_version": "0.2.0",
        "description": "在链外层包裹 prompt",
        "mounts": [{"point": "SingleAgentHook", "implementation": "A"}],
    }


class A(ChainSingleAgentHook):
    async def on_prompt(self, ctx: Context, prompt: str, *,
                        next: HookNext[str, str]) -> str:
        result = await next(prompt + "\nA 前置")
        return result + "\nA 后置"
