# FAE 服务、会话与协议

## 服务与客户端

`fae` 后台服务持有 Engine、Session 和 Timer 调度器。TUI 和直接 CLI 命令是 WebSocket 客户端；飞书网关是独立进程，也作为客户端提交会话操作。关闭客户端只解除连接和订阅，活动执行继续运行。

业务命令按需启动后台服务，默认监听 `127.0.0.1:6666`。`fae server start/status/show/stop` 管理服务；`stop` 等待取消与清理。端口占用直接报错。`fae restart` 在存在有效飞书配置时按网关、服务的依赖顺序停止并启动两者，并保留运行中服务的监听地址和端口。具体命令见 [CLI 文档](../../app/fae/README.md#background-service)。

`--fae-home` 或 `FAE_HOST` 选择实例目录，默认为 `~/.fae`。`server.config` 用于客户端发现运行实例；`server/` 存放服务 token、进程锁、启停锁和服务日志。服务由 CLI 按需启动，未注册为系统服务。

## 执行与存储

| 概念 | 职责 |
| --- | --- |
| Session / `ContextData` | 逻辑身份是 `(可信 user_id, session_id)`；跨执行保存会话配置和可查询视图 |
| Execution | 一次显式 `start`；固定目标定义与指纹、workspace 和 Engine 代际，同一 Session 同时最多一个 |
| Invocation / `Ctx` | 顶层 Plan、子 Plan、Runtime 等调用构成执行树；派生调用继承可信身份和工作区，使用独立局部作用域 |
| Plan / Runtime | Plan 决定下一批任务、推进或结束；Runtime 按 ID 和任务类型执行具体操作 |

`create`、`get` 和 `subscribe` 不启动工作。`start` 校验输入并返回受理结果，业务结果通过 `execution/get` 或 `finished` 获取；忙时再次 `start` 返回冲突。Agent 接受文本或图片内容；Workflow 接受一个 JSON 内容块。Agent 运行期间可用 `steer` 明确提交补充输入，受理后状态先为 `Queued`，实际处理后变为 `Applied` 或 `NotApplied`；Workflow 不支持 `steer`。`stop` 校验预期的 `execution_id` 并传播取消，不回滚已发生的外部副作用。

执行通常从 `Preparing` 进入 `Running`、`Finishing` 和 `Succeeded`、`Failed` 或 `Cancelled`。取消进入 `Stopping`；终态在 Plan、后代调用与资源清理后提交，未处理的输入标为 `NotApplied`，此后释放活跃槽。Workflow 节点和子 Agent 在同一次 Execution 中派生 Invocation；Timer 则为每次触发创建新 Session。

`start` 上的目标、workspace 和执行选项只覆盖本次执行。空闲时可更新默认配置、清除历史或归档；归档后仍可读和订阅。workspace 必须是有效的绝对目录，服务可复用已保存的目录，新会话无配置时回退到服务当前目录；TUI 默认明确传入客户端当前目录。运行中的工具和子调用使用固定 workspace，不切换进程 cwd。

| 路径（相对于 FAE home） | 内容 |
| --- | --- |
| `session/<target>/<user>/<session>.jsonl` | 会话历史；子 Agent 按实际 Agent 分区，Workflow 不自动汇总各节点历史 |
| `session/<target>/<user>/<session>.json` | 保存的 workspace 路径；完整 Session 配置和活动执行状态保存在内存 |
| `memory/<user>.jsonl` | 用户长期记忆，与会话历史分开 |
| `requests/<hash>.json` | 宿主写入的外部请求受理事实，用于恢复时判定是否已经受理 |

## WebSocket 协议 `0.1.0`

| 路径 | 用途 |
| --- | --- |
| `/api/0.1.0/control` | 服务状态与控制、插件管理、公开 Plan/Runtime 调用、流读取与关闭 |
| `/api/0.1.0/session` | 常规会话的创建、配置、历史、执行和订阅 |
| `/api/0.1.0/lark/session` | 仅回环连接，使用独立网关 token 和 `x-fae-lark-principal` 绑定飞书会话身份 |

握手使用 `Authorization: Bearer <token>`，服务拒绝含 `Origin` 的连接。公共请求必须包含 `api_version`、`request_id`、`event`、`session`、`plan`、`payload`。`session` 和 `plan` 不适用时为 `null`；Runtime 调用用 `call: {"runtime_id":"…"}`。响应的 `response_id` 等于原 `request_id`，`msg` 提供说明。成功时 `code` 为 `0`，业务值直接放在 `payload`；错误时 `payload` 为 `null`。响应保留 `api_version`、`session` 和 `plan`，没有 `event`，客户端用原请求选择响应类型。

创建 Agent 会话的请求如下。会话目标放在 `plan.plan_id`，业务配置放在 `payload`，不传 `payload.target`。Workflow 目标使用 `plan_id: "workflow"` 和 `payload.workflow_id`。`execution/start` 和 `config/update` 若不覆盖目标则传 `plan: null`。

```json
{
  "api_version": "0.1.0",
  "request_id": "create-1",
  "event": "/request/session/create",
  "session": null,
  "plan": {"plan_id": "single_agent"},
  "payload": {"name": "Demo", "agent_id": "fae"}
}
```

后续会话请求使用响应关联的 `session.session_id`。

下表列出公共请求前缀及其操作名；完整字段见 [协议事件](../../crates/fae-agent/src/protocol/event.rs)、[业务类型](../../crates/fae-agent/src/protocol/payload.rs) 与 [信封校验](../../crates/fae-agent/src/protocol/codec.rs)。

| 前缀 | 操作 |
| --- | --- |
| `/request/engine/` | `hello`、`status`、`initialize`、`shutdown`、`plugin/show`、`plugin/install`、`plugin/uninstall` |
| `/request/session/` | `create`、`get`、`config/update`、`conversation/list`、`conversation/clear`、`execution/start`、`execution/steer`、`execution/stop`、`execution/get`、`subscribe`、`unsubscribe`、`archive` |
| `/request/plan/` | `call` |
| `/request/runtime/` | `exec`、`select` |
| `/request/stream/` | `next`、`close` |

推送使用 `/event/session/…`。事件的 `session` 包含 `session_id`、`stream_id`、`seq`；客户端用 `execution_id`、`invocation_id`、`output_id` 和 `source` 关联输出。订阅返回 `snapshot` 和 `snapshot_seq`；同一服务实例内可提交 `stream_id`、`after_seq` 续收。回放上限为 4096 项或 16 MiB，游标越界返回 `resume_gap`。服务重启不会重跑中断的 Execution，客户端也不会自动重发未确认的写请求。

主要实现见 [会话协调器](../../crates/fae-engine/src/session/coordinator.rs)、[服务会话入口](../../app/fae/src/server/session.rs)、[控制入口](../../app/fae/src/server/control.rs) 和 [回放缓存](../../app/fae/src/server/replay.rs)。
