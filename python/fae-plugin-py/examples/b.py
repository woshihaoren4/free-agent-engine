from fae_plugin import ChainSingleAgentHook, Context, HookNext


def plugin_config() -> dict:
    return {
        "registration_mode": "chain",
        "plugin_version": "1.0.0",
        "description": "包裹下一层，或在命中条件时短路",
        "mounts": [{"point": "SingleAgentHook", "implementation": "B"}],
    }


class B(ChainSingleAgentHook):
    async def on_prompt(self, ctx: Context, prompt: str, *,
                        next: HookNext[str, str]) -> str:
        if "直接处理" in prompt:
            return prompt + "\nB 已直接处理"
        return (await next(prompt + "\nB 前置")) + "\nB 后置"
