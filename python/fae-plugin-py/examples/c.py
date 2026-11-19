from fae_plugin import ChainSingleAgentHook, Context, HookNext


def plugin_config() -> dict:
    return {
        "registration_mode": "chain",
        "plugin_version": "1.0.0",
        "description": "Python 链的终端处理",
        "mounts": [{"point": "SingleAgentHook", "implementation": "C"}],
    }


class C(ChainSingleAgentHook):
    async def on_prompt(self, ctx: Context, prompt: str, *,
                        next: HookNext[str, str]) -> str:
        return prompt + "\nC 内容"
