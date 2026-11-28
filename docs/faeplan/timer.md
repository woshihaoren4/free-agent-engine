# FAE 定时器

## 管理接口

后台服务运行持久化 Timer 调度器。为 Agent 启用 `timer` 工具后，可在当前 Agent 和用户分区管理定时任务。归属和执行绑定来自可信 Context，任务输入不能指定其他用户、Agent 或工作区。

| `operation` | 字段 |
| --- | --- |
| `create` | `name`、非空文本 `input`、`schedule`、`idempotency_key`；可选 `misfire_policy`。重试使用同一幂等键 |
| `query` | 可按 `id`、`state` 查询；用 `include_occurrences` 查看触发状态及 Session ID，列表支持 `cursor` 和 `limit` |
| `update` | `id` 和查询得到的 `expected_revision`；可修改定义，`enabled` 控制暂停或恢复，不清空已接受次数 |
| `delete` | `id`，可带 `expected_revision`；删除定义不会取消已经启动的 Session |

管理定时器的完整字段见 [TimerRequest](../../crates/fae-agent/src/timer.rs) 和 [工具参数](../../crates/fae-engine/src/tools/timer.rs)。清理会话历史或 Todo 不会移除独立的定时任务。

## 时间与次数

`schedule.kind` 可为 `once`（接受一次触发）、`repeat`（必须给出正数 `max_fires`）或 `loop`（持续调度）。三种都使用 `cron`，支持 5 字段（分时日月周）、6 字段（前加秒）或 7 字段（末尾加年）；5 字段规范化时补 `0` 秒。语法以 `cron 0.16` 为准。`timezone` 默认 `UTC`，支持 IANA 时区；夏令时跳过不存在的本地时刻，重复时刻取较早的 UTC 时间。时间以 UTC 保存，查询可返回带偏移的 `next_local`。

`misfire_policy` 缺省 `coalesce`，合并到最近一个到期时刻；`skip` 跳过已错过的时刻。当前实现以迟到超过 2 秒判定漏触发。资源紧张或前一次尚未结束时，每个定时任务至多保留一个待处理时刻，多次到期合并，不逐个补跑。`accepted_count` 随 occurrence 的持久接受增加，即使后续失败或取消也计数；仅排队或被跳过的时刻不计数。

## 持久化与调度

定时定义、触发记录和创建幂等键保存在 `<home>/timer/<agent>/<user>/state.json`，以分区为单位保存。调度器先持久化 `pending` 记录及该次触发的 Session ID，再发出内部 Timer 事件。服务监听事件，在容量允许时 claim，使用该 Session ID 通过内部入口启动新的 `chat` Execution。

每次触发使用保存的 Agent 来源和 workspace，启动前重新解析定义并检查绑定；不复用建立定时任务时的会话历史，也不复制原请求附件。新会话仍使用该用户的长期记忆。无效来源、Agent 名称不符或目录不可用时记录失败。

触发记录从 `pending` 经 `dispatching`、`running` 进入成功、失败或取消等终态。服务重启后可以恢复 `pending`，无法确认的 `dispatching` 或 `running` 标为 `interrupted`，不会重跑。删除可抑制未交接的触发。定时定义的 `completed` 只表示不再接受新触发；实际执行结果查 occurrence。

主要实现见 [TimerRuntime](../../crates/fae-engine/src/engine_rt/timer_runtime.rs)、[时间计算](../../crates/fae-engine/src/timer/scheduler.rs)、[分区存储](../../crates/fae-engine/src/timer/store.rs) 和 [服务事件分发](../../app/fae/src/server/timer.rs)。
