"""Installable example; all payloads stay ordinary JSON values."""
import logging
from fae_plugin import ChainPlan, ChainRuntime, PlanNext, TaskMetadata, TaskRequest, TaskResponse


class Audit(ChainPlan):
    def id(self):
        return "audit_plan"

    async def init(self):
        logging.warning("audit_plan: starting report")
        return await self.next_plan.init()

    async def next(self, response):
        logging.warning("audit_plan: received task %s", response.meta.id)
        return await self.next_plan.next(response)


class Report(ChainPlan):
    def id(self):
        return "report_plan"

    async def init(self):
        return PlanNext.Tasks([
            TaskRequest(TaskMetadata("report_1", {"Any": "report_fetch"},
                                     executor="report_runtime"), self.env, self.ctx)
        ])

    async def next(self, response):
        streams = [response.resp["stream_id"]] if isinstance(self.env, dict) and self.env.get("stream") else []
        await self.ctx.complete(response.resp, streams=streams)
        return PlanNext.End()


class ReportRuntime(ChainRuntime):
    def id(self):
        return "report_runtime"

    def tys(self):
        return ["report_fetch"]

    async def select(self, ty, cond):
        return cond

    async def exec(self, task):
        result = task.req
        if isinstance(task.req, dict) and task.req.get("stream"):
            items = list(task.req.get("items", []))

            async def chunks():
                for item in items:
                    yield item

            result = await self.ctx.stream(chunks())
        return TaskResponse(task.meta, result, task.ctx)
