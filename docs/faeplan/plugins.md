# FAE Plan、Runtime 与 Hook 插件

## 安装与声明

当前可执行插件使用 Python 3.10+。SDK 发行版本为 `0.4.0`，由 [pyproject.toml](../../python/fae-plugin-py/pyproject.toml) 定义；插件声明 API 为 `0.2.0`（Hook）或 `0.3.0`（Plan/Runtime）。公共协议与 Worker IPC 的 `api_version` 均为 `0.1.0`，各自使用不同的信封。JavaScript 有声明类型，宿主尚无对应执行器。

将 SDK 装入宿主使用的 Python，必要时在服务启动前设置 `FAE_PLUGIN_PYTHON`。CLI 已启用 Python 支持；作为 Rust 库使用时启用相应 crate 的 `python` feature。通过 `fae plugin install <路径>`、`fae plugin show [ID]` 和 `fae plugin uninstall <ID>` 管理插件；安装记录保存在 `<home>/plugins/plugins.config`。

目录插件使用 UTF-8 JSON `plugin.config`，`implementation` 为 `相对文件.py:类名`。单文件插件导出同步、无参、返回字典的 `plugin_config()`，`implementation` 只写类名。检查与 Worker 分别导入单文件，模块顶层代码可能执行两次。目录插件直接读取配置，不调用 `plugin_config()`。加载时校验入口、类和挂载；Worker 读取源文件，源码未被冻结。

声明需要 `plugin_version`、`description`、`mounts`，目录插件还须声明 `engine: "py"`。`registration_mode` 缺省为 `direct`，`fae_plugin_api_version` 缺省为 `0.2.0`。插件 ID 与各 Plan/Runtime 的 ID 是不同的身份；Plan/Runtime 插件须显式声明插件 ID 与 API `0.3.0`。插件 ID 格式为 `[a-z][a-z0-9_]{0,63}`。

| 挂载点 | 模式与定位 |
| --- | --- |
| `Plan` | 仅 `chain`；`id` 全局唯一，同一插件也不能重复；可通过 `next_plan_id` 显式调用下一层 |
| `Runtime` | `direct` 或 `chain`；固定 `id`，声明字符串数组 `task_types`；如 `"Model"` 对应内置类型，自定义名称对应 `TaskType::Any` |
| `SingleAgentHook` | `direct` 或 `chain`；按 `agent_id` 精确匹配，省略时全局 |
| `ToolsHook` | `direct` 或 `chain`；按 `channel` 路由，工具名使用 `channel__name` |
| `ModelHook` | `direct` 或 `chain`；由 `match_model` 动态判定，多个独立节点同时命中时报告冲突 |

## Plan、Runtime 与公开桥接

Rust Engine 的 `build_plan`、`execute_plan`、`execute_runtime`、`select_on` 接受真实 `Ctx` 和调用元信息，内部保留原生 Rust 类型。`EngineAdapters` 将 Engine 和 `BridgeRegistry` 作为同一代际发布。Bridge 按 Plan ID，或 Runtime ID、TaskType 与操作选择明确的编解码契约；JSON 转换由适配层完成。

Python `ChainPlan` 实现 `id/init/next/abort`。每次 Plan 运行创建独立实例；`self.env`、`TaskRequest.req` 与 `TaskResponse.resp` 直接承载 JSON 值，任务元信息与 Context 单独传递。子 Plan 任务用 `ty=Plan`、`target_plan_id` 和目标 `env`。宿主在发送远端 create 前登记释放路径，并为未能正常完成清理的情况预留延迟释放。

Python `Runtime` 和 `ChainRuntime` 由 `EngineRuntime` 统一管理。调用方可按 ID 路由，也能按 TaskType 调度。公开 Runtime 请求按 `call.runtime_id` 和操作路由；单类型 Runtime 默认公开 `exec`，`select` 用 `public_calls.select` 显式声明，多类型 Runtime 要为每个公开操作指定类型。配置通过不代表对应 Python 方法一定实现，调用方仍须处理操作失败。Python Worker 的宿主子 Plan 调用读取 `Value` 结果，被调用的原生 Plan 应提供相应的 JSON 结果类型。

[控制 WebSocket](service-and-session.md#websocket-协议-010) 的 `/request/plan/call` 把 `payload` 原样传给公开 Plan 的 `env`，`/request/runtime/exec` 与 `/request/runtime/select` 把它传给公开 Runtime。控制入口创建独立根 Context。公开 Plan 的结果必须能读取 JSON 快照；原生 Plan 可以在内部返回其他 Rust 类型。可运行的 [Plan/Runtime 示例](../../examples/plugin/plan_runtime/README.md) 展示 `audit_plan → report_plan → report_runtime`，包含 JSON 和流请求。

## Hook、Worker 与流

`direct` Hook 由 Rust 按插件安装顺序调用，每个插件使用独立 Worker；`chain` 成员共用 Worker，在首个 chain 成员的安装位置进入 Python 洋葱链。`Chain*Hook` 的异步方法通过 `await next(value)` 进入下一层；直接返回会短路当前方法链。工具链和模型链需要实际处理者，链尾没有隐式原生处理器。

`SingleAgentHook` 可处理查询、记忆、历史、Prompt、压缩、模型请求/响应、工具请求/响应和保存。`ToolsHook` 提供 `desc/exec`，`ModelHook` 提供 `match_model/exec`。SDK 的 `Context` 可读取概要或导出上下文；`Session.send()` 发送独立的结构化输出，不自动写入会话历史；`complete_query()` 可在 `on_query` 结束当前执行。

Worker 调用能力绑定当前 Context 和代际。Provider 的 `ctx`、`rt` 与 continuation 只在当次调用有效；不要保存到后续回调。异步方法运行在事件循环；受支持的同步方法使用有界线程池。Worker 进程与模块命名空间用于隔离执行和状态，不是权限沙箱。

模型完整结果和流分别使用 `{"kind":"completed","response":…}` 和 `{"kind":"streaming","stream_id":…}`。Python 可用 `await self.ctx.stream(iterator)` 注册 async iterator，并用 `stream_next/stream_close` 消费或关闭；Plan 可跨回调保存流 ID，使用新调用能力继续消费。对外返回流时用 `complete(result, streams=[id])` 显式转交租约；公共消费者通过 `/request/stream/next` 和 `/request/stream/close`，以 `owner_request_id` 与 `stream_id` 定位原结果。流有容量、并发和有效期限制，最长 120 秒；迭代器不能使用已过期的调用能力。

在线安装、覆盖或卸载先校验候选 Worker 和完整 Engine/Bridge，再发布新快照。正在执行的 Context 保留原代际，后续执行使用新快照。服务启动从当前源码加载已启用插件；加载错误会使启动失败。卸载更新安装记录并保留源码。

开发入口：[Python SDK](../../python/fae-plugin-py/README.md)、[声明校验](../../crates/fae-agent/src/plugin/mod.rs)、[插件管理器](../../crates/fae-agent/src/plugin/manager.rs)、[Engine 注册](../../crates/fae-engine/src/engine/engine_builder.rs)。
