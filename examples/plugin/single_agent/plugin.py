"""A small Agent hook; logs go to stderr, messages go to the actual session."""
import logging
import sys

from fae_plugin import Context, SessionMessage, SingleAgentHook, TextContent


class DemoAgentHook(SingleAgentHook):
    def __init__(self) -> None:
        self.log = logging.getLogger("fae.example.single_agent")
        self.log.setLevel(logging.INFO)
        self.log.propagate = False
        if not self.log.handlers:
            handler = logging.StreamHandler(sys.stderr)
            handler.setFormatter(logging.Formatter("[single-agent-plugin] %(message)s"))
            self.log.addHandler(handler)

    async def on_query(self, ctx: Context, query: str) -> str:
        summary = await ctx.read_summary()
        data = await ctx.export_context()
        execution = data.latest_execution_view
        invocation = next((item for item in execution.invocations
                           if item.invocation_id == ctx.invocation_id), None) if execution else None
        agent_id = invocation.agent_id if invocation else None
        self.log.info(
            "agent=%s execution=%s invocation=%s user=%s state=%s workspace=%s",
            agent_id, ctx.execution_id, ctx.invocation_id, summary.user_info.user_id,
            execution.status.value if execution else None,
            execution.effective.workspace if execution else summary.session_config.workspace.root,
        )
        if ctx.session is not None:
            receipt = await ctx.session.send([
                TextContent(f"[single-agent-plugin] agent={agent_id}，已收到请求，开始处理。")
            ])
            self.log.info("session=%s output=%s", ctx.session.session_id, receipt.output_id)
        return query.strip()

    async def on_prompt(self, ctx: Context, prompt: str) -> str:
        self.log.info("on_prompt invocation=%s", ctx.invocation_id)
        return prompt + "\n回答保持简洁，使用用户输入的语言。"

    async def on_save(self, ctx: Context, messages: list[SessionMessage]) -> list[SessionMessage]:
        self.log.info("on_save invocation=%s messages=%d", ctx.invocation_id, len(messages))
        return messages
