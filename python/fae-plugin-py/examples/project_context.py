from fae_plugin import Context, SingleAgentHook, TextContent


def plugin_config() -> dict:
    return {
        "plugin_version": "1.0.0",
        "fae_plugin_api_version": "0.2.0",
        "description": "清理输入并为 fae Agent 补充项目规范",
        "mounts": [{
            "point": "SingleAgentHook",
            "implementation": "ProjectContext",
            "agent_id": "fae",
        }],
    }


class ProjectContext(SingleAgentHook):
    async def on_query(self, ctx: Context, query: str) -> str:
        if ctx.session is not None:
            await ctx.session.send([TextContent("开始处理请求。")])
        return query.strip()

    async def on_prompt(self, ctx: Context, prompt: str) -> str:
        return prompt + "\n遵守当前项目的代码规范。"
