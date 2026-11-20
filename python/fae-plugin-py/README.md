# fae-plugin-py

FAE 插件 API `0.1.0` 的 Python SDK，支持 Agent Hook、工具和完整/流式模型。
Python 3.10 及以上可用。发行名是 `fae-plugin-py`，导入名是 `fae_plugin`。

```bash
python3 -m pip install -e ./python/fae-plugin-py
# 示例模型额外依赖
python3 -m pip install httpx
```

## 打包和发布到 PyPI

在仓库根目录执行：

```bash
# 仅打包并校验，不上传，也不需要 Token
bash python/publish.sh --build-only

# 打包、校验并发布到正式 PyPI；按提示隐藏输入 API Token
bash python/publish.sh
```

自动化发布可通过环境变量 `PYPI_API_TOKEN` 或 `TWINE_PASSWORD` 提供 Token。
脚本顶部列出了所有支持的环境变量。测试发布时设置
`TWINE_REPOSITORY_URL=https://test.pypi.org/legacy/`，并使用 TestPyPI 的 Token。
构建产物默认保存在本包的 `dist/`，上传只使用本次构建的 wheel 和源码压缩包。
每次发布前更新 `pyproject.toml` 中的 `version`，PyPI 不允许覆盖同版本文件。

发布成功后，其他人可安装并导入：

```bash
python3 -m pip install fae-plugin-py
```

```python
from fae_plugin import Context, SingleAgentHook, ToolsHook, ModelHook
```

宿主从 `FAE_PLUGIN_PYTHON` 选择解释器，默认 `python3`。该解释器需要预先安装 SDK
和插件业务依赖；加载插件不会执行 pip。FAE CLI 已启用 Rust `python` feature；
独立使用 fae-agent/fae-engine 时需显式启用相应 feature。

## 创建和加载

将单文件放在 `<fae_home>/plugins/project_context.py`：

```python
from fae_plugin import SingleAgentHook

def plugin_config():
    return {
        "plugin_version": "1.0.0",
        "description": "清理输入并补充项目规范",
        "mounts": [{"point": "SingleAgentHook", "implementation": "ProjectContext",
                    "agent_id": "fae"}],
    }

class ProjectContext(SingleAgentHook):
    async def on_query(self, ctx, query):
        if ctx.session is not None:
            await ctx.session.send("开始处理请求。")
        if query.strip() == "/stop":
            await ctx.cancel_run("用户要求停止")
        return query.strip()

    async def on_prompt(self, ctx, prompt):
        return prompt + "\n遵守项目代码规范。"
```

```bash
fae plugin install /path/to/project_context.py
fae plugin show project_context
fae plugin show '*'
fae --fae-home /path/to/.fae plugin uninstall project_context
```

`fae plugin show` 不传 ID 时等同于 `fae plugin show '*'`，简洁列出全部插件。

安装命令将插件文件或目录复制到 `<fae_home>/plugins/`，同名覆盖并保留安装位置。
固定 JSON 文件 `plugins/plugins.config` 的 `plugins` 数组记录安装顺序、
`id`、`source`、`installed` 和 `enabled`；卸载保留源码并将两个状态设为 false，
卸载后重装追加到末尾。可编辑 `enabled` 禁用插件。
服务启动按配置顺序加载已安装且启用的当前源码，任何错误都会导致启动失败。
服务运行时安装、覆盖和卸载即时更新运行时，无需重启；已有任务保留原 Worker 快照。
服务未运行时只更新文件及配置，下次启动生效；未记录在配置中的源码不会自动加载。

单文件配置函数是同步无参数函数，每次服务加载调用一次。目录插件以根目录
`plugin.config` UTF-8 JSON 为唯一配置，必须有 `"engine": "py"`，入口为
`"implementation": "plugin.py:ProjectContext"`，不调用配置函数。

声明的必填字段为 `plugin_version`（SemVer）、`description`、`mounts`；
`registration_mode` 默认 `direct`，`fae_plugin_api_version` 默认 `0.1.0`。
未知字段、不匹配的接口位置、重复挂载、越界入口或不兼容版本会拒绝加载。
JavaScript 引擎首期明确不可用。

`on_query` 在本次用户输入的其他 Agent Hook 之前执行，包括补充输入。
返回清理后的输入；图片等附件保持绑定。内部 todo 推理提示不重复调用它。
Agent 其他方法与 Rust 对应：`on_prompt`、`on_memory`、`on_history`、
`on_compression`、`on_model_req`、`on_model_resp`、`on_tools_req`、
`on_tools_resp`、`on_save`。direct 基类默认透传。

## direct 与 chain

direct 每个插件一个 Worker，Rust 按插件 ID 排序传递 Agent 返回值。
chain 每个 home 一个 Worker 链，按第一次成功注册的持久化顺序执行。
重新加载保留位置；链根是第一个成员。两种模式可以共存。

chain 声明 `"registration_mode": "chain"`，继承 `ChainSingleAgentHook`、
`ChainToolsHook` 或 `ChainModelHook`，方法必须 async 并有必需关键字参数 `next`：

```python
from fae_plugin import ChainSingleAgentHook, Context, HookNext

class Surround(ChainSingleAgentHook):
    async def on_prompt(self, ctx: Context, value: str, *,
                        next: HookNext[str, str]) -> str:
        result = await next(value + "\n前置处理")
        return result + "\n后置处理"
```

不调用 next 并直接返回合法值会短路链的内层。next 只能在当前方法中由当前
async task 调用一次；不能保留到流生成器、其他 task 或下一轮。Agent 链默认
转发，链末尾返回输入；工具和模型链末尾没有原生实现，必须有成员覆盖业务方法。
`ChainModelHook.match_model(ctx, model)` 是独立判断方法，不接收 `next`，
每个模型类都必须实现；只有返回 True 的成员进入本次模型链。

## 上下文、状态和栈

所有接口接收同一种 Context。属性 `agent_id/user_id/scope_id/run_id` 和
Agent 的 `plan_id/task_id/turn_id` 是只读身份缓存。`ctx.session` 可能为 None。
`get_engine()` 返回具有 `engine_id` 和 `same_engine()` 的宿主引用。

```python
from fae_plugin import (
    ContextChildSpec, ContextState, ContextStackQuery,
    ContextStackDraft, ContextStackPhase, TaskMeta, TaskType,
)

async def inspect(ctx):
    scope = await ctx.scope()
    child = await ctx.fork(ContextChildSpec())  # 只创建，不调度任务
    await child.set_state(ContextState.running())
    view = child.state()
    try:
        print((await view.current()).kind)
        await child.cancel("关闭临时子上下文")
        await view.changed()
    finally:
        await view.aclose()
    await ctx.append_stack(ContextStackDraft(
        TaskMeta("plugin-step", ctx.plan_id or "", TaskType(), "plugin", "inspect"),
        ContextStackPhase.STARTED,
    ))
    entries = await ctx.stacks(ContextStackQuery(tree=True, limit=20))
    if entries:
        next_page = await ctx.stacks(ContextStackQuery(
            tree=True, after_sequence=entries[-1].sequence, limit=20))
```

`state()` 创建订阅视图，`current()` 不消费执行结果。成功状态的 output 支持
`await output.json_snapshot()`；不可编码原生结果报 `UnsupportedOutputType`。
Agent 计划与插件创建的子上下文可写 JSON 成功输出，原生工具/模型任务不能写入伪造 JSON 结果。
`cancel()` 取消当前上下文；`cancel_run()` 取消固定 root Run；取消子上下文不会
自动取消父 Hook。终态写入不可恢复执行。

`bind_scope` 保留原生一次性绑定语义：已绑定 Hook 拒绝重绑，静态 ContextNull
不能绑定。session/todo 等宿主引用不能通过字符串伪造。`fork(execution=True)`
遵循 Rust 的执行归属清空/显式指定规则，用户始终继承。

代理限于当前调用与关联流；方法返回后无流则释放，调用结束时取消未调度的
临时子上下文。不要把 ctx/session/next 保存到实例属性用于其他输入。

`session.send(text)` 产生独立 `plugin_message`，CLI/TUI/Lark 会展示；它不追加
模型历史，也不修改最终答案。静态工具发现 ContextNull 无 session，不能取消任务。

## 工具与模型

ToolsHook 声明 `channel`，默认 `fae_<plugin_id>`。类须实现 `desc/exec`；
`channel()` 是绑定值，工具名沿用 `channel__name`：

```python
from fae_plugin import ToolsHook, ToolResponse

class Echo(ToolsHook):
    async def desc(self, ctx, tool_name):
        return {"name": tool_name, "description": "返回文本",
                "parameters": {"type": "object", "properties": {
                    "text": {"type": "string"}}, "required": ["text"]}}

    async def exec(self, ctx, request):
        return ToolResponse.completed(request.arguments_json()["text"])
```

加载后仍需在 Agent `tools` 配置中选择工具。工具参数保留 JSON 字符串形式，
工具流首期仅透传。已有工具 channel 冲突时拒绝候选版本。

ModelHook 挂载只声明接口和实现类，不接受 `model` 字段。类必须实现
`match_model(ctx: Context, model: str) -> bool`，根据请求中的模型名称判断
是否接管。direct 判断方法可同步或异步，chain 判断方法须异步且不接收 next。
判断应保持轻量、无副作用，必须返回 bool。宿主先完成各候选的判断，再执行
唯一匹配节点的 exec；不会因 exec 抛错而尝试其他模型或重放请求。

```python
class MyModel(ModelHook):
    async def match_model(self, ctx: Context, model: str) -> bool:
        return model == "my-model" or model.startswith("my-model/")

    async def exec(self, ctx: Context, request: ModelRequest) -> ModelResponse:
        # 在此实现完整/流式响应。
        ...
```

独立节点（含 direct 与 chain 混用）同时命中时报 `plugin_model_conflict`，
同一个 direct 插件内多个模型类同时命中也报冲突；chain 内命中的多个层按
持久化顺序执行。没有命中时普通模型使用原生 Runtime，`plugin:` 名称没有命中
时报 `plugin_model_not_found`。因此插件可以显式匹配普通模型名并接管该请求，
不再在加载时为模型预留名称或检查静态名称冲突。运行中的请求使用固定插件快照。

Agent 的 `model.model` 仍填写模型名称；例如回显示例匹配
`plugin:echo_model/default`，HTTP 示例匹配 `plugin:http_model/default`。
模型必须处理 `request.stream` 两种路径，流返回
`ModelResponse.streaming(async_iterator)`；完整响应返回
`ModelResponse.completed(openai_response_dict)`。chunk 使用 OpenAI Chat 格式，
支持文本、tool_calls、多个 choice、finish_reason 和 usage-only chunk。
全部 choice 结束后才可 EOF。宿主不自动重试插件模型。

[examples/http_model](examples/http_model) 展示真实异步 HTTP 模型调用；
`on_model_resp` 可返回输入响应或包装其 async iterator。

从旧版本升级时，在模型类中补充 match_model，删除 plugin.config 或
plugin_config() 中的模型挂载 model 字段，重新加载插件。宿主读取旧持久化
记录时会迁移旧 model 元数据，但不会修改冻结源码；未实现新接口的旧执行版本
会显示加载错误，必须用更新后的源码重新加载。

可直接复制的样例还包括 `examples/project_context.py`（direct Agent）、
`examples/echo_tools/`（目录工具插件）、`examples/a.py`、`b.py`、`c.py`
（Python 洋葱链）。将样例复制到 `<fae_home>/plugins/` 后依次执行
`fae plugin a`、`fae plugin b`、`fae plugin c`，得到 A→B→C；
工具样例加载为 `fae plugin echo_tools`，Agent 的 tools 中选择
`fae_echo_tools__echo`。

## 运行和开发

同一实例可并发处理不同 session，direct 同步方法在有界线程池运行；
chain 方法在事件循环中执行。共享实例只能保存支持并发的客户端或受短锁保护的
缓存。异步方法不要调用阻塞 HTTP/IO。

例如共享查询缓存只在读写字典时加锁，HTTP 等待位于锁外；身份从本次 ctx 读取：

```python
import asyncio
import httpx

class LookupCache:
    def __init__(self):
        self.cache = {}
        self.lock = asyncio.Lock()

    async def lookup(self, ctx, url):
        session = ctx.session  # 调用局部变量
        key = (ctx.user_id, session.session_id if session else None, url)
        async with self.lock:
            if key in self.cache:
                return self.cache[key]
        async with httpx.AsyncClient() as client:
            response = await client.get(url)
            response.raise_for_status()
            text = response.text
        async with self.lock:
            if len(self.cache) >= 128:
                self.cache.pop(next(iter(self.cache)))
            self.cache[key] = text
        return text
```

普通方法及模型 match_model 判断预算默认 1 秒，模型 exec 和模型流总预算 120 秒；排队计入预算。
首块等待上限 30 秒，后续空闲上限 60 秒；消费者不读取的时间仅计入总预算。
Worker 使用有界队列、8 个业务槽、每组 4 个槽、4 个同步线程及有界流引用。
流逐块拉取，消费端 drop/cancel 释放调用。stdout 专用于协议，print 转到 stderr。

启动时校验当前配置、类、签名、依赖和路由，错误直接向上传递。
宿主不保存冻结源码或恢复旧版本。安装状态只记录在 `plugins/plugins.config`，
重启重新读取当前源码及声明；离线 show 不运行插件代码，单文件声明显示为未知。

```bash
PYTHONPATH=python/fae-plugin-py/src python3 -m unittest discover \
  -s python/fae-plugin-py/tests -v
PYTHONPATH=python/fae-plugin-py/src cargo test -p fae-engine \
  --features python plugin_tests
```
