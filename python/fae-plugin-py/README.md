# fae-plugin-py

FAE Agent Engine 的类型化 Python 插件 SDK，支持 Plan、Runtime，以及 Agent、工具和模型钩子，
以及执行上下文读取、Session 输出和子调用。

需要 Python 3.10 或更新版本。SDK 的发行版本与插件 API 版本分别管理；
SDK 版本为 `0.4.0`；Plan/Runtime 声明插件 API `0.3.0`，
既有 Hook 声明 `0.2.0`。传输协议固定为 `api_version: "0.1.0"`，
需要对应版本的 FAE 宿主。

## 安装

```bash
python3 -m pip install fae-plugin-py
```

在仓库根目录进行本地开发：

```bash
python3 -m pip install -e ./python/fae-plugin-py
```

将 SDK 安装到宿主使用的 Python 解释器中。若使用虚拟环境，在启动 FAE 服务前
设置 `FAE_PLUGIN_PYTHON` 为该环境的 Python 路径。

## 最小 Agent 插件

保存为 `project_context.py`：

```python
from fae_plugin import Context, SingleAgentHook, TextContent


def plugin_config() -> dict:
    return {
        "plugin_version": "1.0.0",
        "fae_plugin_api_version": "0.2.0",
        "description": "清理输入并补充项目规范",
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
```

安装插件并使用已配置的 `fae` Agent：

```bash
fae plugin install ./project_context.py
fae agent --agent-id fae
```

目录插件通过 `plugin.config` 声明挂载点，`implementation` 使用
`plugin.py:ClassName` 格式；单文件插件通过同步的 `plugin_config()` 返回配置。

## 主要接口

- `ChainPlan`：`id/init/next/abort` 对齐 Rust Plan，`self.env` 为原始 JSON；
  Plan 仅支持 chain，使用 `next_plan_id` 串联不同且全局唯一的 ID。
- `Runtime`、`ChainRuntime`：支持 direct/chain，声明固定 `id()` 和 `tys()`；
  `exec(TaskRequest)` 返回保留原 meta/ctx 的 `TaskResponse`，`select` 返回 JSON。
- `TaskRequest.req`、`TaskResponse.resp`：直接传递 JSON 值，不增加业务载荷包装。
- Provider 的 `self.ctx`、`self.rt`、continuation 是调用局部能力；
  Plan 状态可保留 stream ID，下一次调用用新的 `self.ctx` 消费，不能保存旧 Context 能力。
- `await self.ctx.stream(iterator)`：注册有界 async iterator，返回 stream reference；
  `stream_next(id)` 消费、`stream_close(id)` 关闭。Plan 发布流结果时使用
  `await self.ctx.complete(result, streams=[id])`，将租约转交接收方。
- `SingleAgentHook`：处理输入、提示词、记忆、历史、模型请求与响应、工具请求与响应。
- `ToolsHook`：实现 `desc` 和 `exec`，提供工具描述和执行结果。
- `ModelHook`：实现 `match_model` 和 `exec`，提供模型响应。
- `ChainSingleAgentHook`、`ChainToolsHook`、`ChainModelHook`：通过 `HookNext`
  将调用传给后续插件，配置 `registration_mode: "chain"`。
- `Context`：通过 `read_summary()` 和 `export_context()` 读取公共上下文；
  `execution_id` 和 `invocation_id` 标识当前调用。
- `Session.send()`：发送由 `TextContent`、`JsonContent` 或 `ResourceContent`
  组成的内容，并返回输出回执；Session 可能为空，使用前需检查。
- `Context.complete_query(text)`：在 `on_query` 的原始上下文中发送最终回答并结束执行；
  此方法不返回，后续查询钩子和模型任务不会继续执行。

## 示例与源码

Plan/Runtime 示例见 [plan_runtime](../../examples/plugin/plan_runtime/README.md)；
协议、原生类型桥接和生命周期见 [插件实现说明](../../docs/faeplan/plugins.md)。

完整示例见 [插件示例](https://github.com/woshihaoren4/free-agent-engine/tree/v2-plugins/examples/plugin)
和 [SDK 示例](https://github.com/woshihaoren4/free-agent-engine/tree/v2-plugins/python/fae-plugin-py/examples)。
项目使用 Apache-2.0 许可证。
