"""A small Agent hook; logs go to stderr, messages go to the actual session."""
import logging
import sys

from fae_plugin import Context, SessionMessage, SingleAgentHook


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
        scope = await ctx.scope()
        view = ctx.state()
        try:
            state = await view.current()
        finally:
            await view.aclose()
        self.log.info(
            "agent=%s run=%s user=%s state=%s workspace=%s",
            ctx.agent_id, ctx.run_id, ctx.user_id, state.kind.value, scope.workspace,
        )
        if ctx.session is not None:
            receipt = await ctx.session.send(
                f"[single-agent-plugin] agent={ctx.agent_id}，已收到请求，开始处理。"
            )
            self.log.info("session=%s message=%s", ctx.session.session_id, receipt.message_id)
        return query.strip()

    async def on_prompt(self, ctx: Context, prompt: str) -> str:
        self.log.info("on_prompt agent=%s", ctx.agent_id)
        return prompt + "\n回答保持简洁，使用用户输入的语言。"

    async def on_save(self, ctx: Context, messages: list[SessionMessage]) -> list[SessionMessage]:
        self.log.info("on_save agent=%s messages=%d", ctx.agent_id, len(messages))
        return messages
