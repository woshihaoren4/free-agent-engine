# FAE 飞书网关

## 运行与配置

`fae lark` 是独立进程，通过自建应用机器人的长连接接收 `im.message.receive_v1`，再通过回环 WebSocket `/api/0.1.0/lark/session` 提交给后台服务。停止网关不停止共享服务。飞书应用需启用机器人、配置长连接事件并发布，授予单聊及群聊 @ 消息接收、机器人发送消息、表情回应、CardKit 和按需读取消息资源的权限。

`fae lark init` 创建 `<home>/lark.config`；填写 `app_id`、`app_secret`、`tenant_key`、`agent_id`，并确认 `workspace` 是可用目录（默认 `~`）。用 `fae lark check` 检查配置及 Agent 依赖，`check --online` 额外检查机器人凭据，`show` 隐去密钥。通过 `start`、`status`、`stop` 管理进程。完整命令见 [CLI 文档](../../app/fae/README.md#feishu-gateway)。

`user_id_whitelist` 采用发送者的 `open_id`；省略或 `null` 表示不限，空数组表示全部拒绝。`fae lark whitelist add/remove/set/reload` 可在线更新后续准入。其他租户和机器人消息被忽略，群聊中无论是否在话题内都必须真实 @ 当前机器人。

## 输入与话题

网关支持文本、富文本（`post`）、可解析内容的卡片（`interactive`）及图片，包括纯图片输入。富媒体转为文本和图片资源；飞书图片在提交前以机器人凭据下载并转成内联资源。单条最多 20 张，下载及总消息还有大小限制；不支持的消息类型或无法解析的内容会收到明确反馈。

话题外的普通输入先发送 `Typing` 话题回复，并验证真实 thread ID 后才提交 Agent。缺失或无法确认 thread ID 时不会执行；普通引用消息不会因为携带根消息 ID 而续接其他话题。话题内按 `(sender open_id, thread_id)` 复用逻辑 Session，分别作为 FAE `user_id` 和 `session_id`。例如同一发送者的两个话题历史独立，用户记忆按发送者共享。可信绑定还限定应用、租户、聊天、实际 Agent 和 workspace。

| 场景 | 行为 |
| --- | --- |
| 空闲话题 | 普通输入开始执行；`/todo <message>` 进入规划；`/status`、`/session` 查询当前话题；`/clean` 或 `/session clean` 清理历史和 Todo，保留绑定 |
| 忙碌话题 | 普通输入和大部分命令文本立即作为 `steer`；`/steer <message>` 显式补充；`/stop` 取消当前执行，空闲时忽略 |
| 会话切换 | `/session new` 提示在话题外发新消息；`/session id=<id>` 提示去对应话题回复；话题外管理命令不操作旧 Session |

网关默认最多同时接纳 4 个新执行（可配置为 1–8）。达到限制时提示重试，不排队等待新一轮；已经运行的会话仍可接收 `steer`。

## 输出与恢复

话题内已接收的输入使用 `Typing` 表情回应。输出按 Invocation 和模型响应区分，插件的结构化输出独立投递。网关在执行结束后发送最终卡片，长答案分页，CardKit 失败时可降级为文本；成功投递后清理相关 `Typing` 提示。模型正常输出留在已确认的话题内。

当前根 Agent 的 System 包含独立的 `LarkEnvironment` 段，描述身份、话题、消息关系与 @ 元数据。补充输入受理后，下一次模型请求可刷新这一段。子 Agent、Workflow 节点和 Timer 执行不会继承该飞书消息段。

`<home>/lark/records` 保存收件及投递回执，默认保留已完成记录 7 天；`lark/threads`、`lark/roots`、`lark/service-bindings` 独立保存话题和服务绑定。网关按原服务实例、stream 和序号恢复订阅。回放断档时尝试读取最终结果；受理不明时查询宿主的受理事实，不盲目重跑。未完成投递根据持久记录恢复。

主要实现见 [输入解析](../../app/fae/src/lark/inbound.rs)、[富媒体输入](../../app/fae/src/lark/content.rs)、[话题创建和确认](../../app/fae/src/lark/thread.rs)、[会话路由](../../app/fae/src/lark/session.rs) 和 [输出处理](../../app/fae/src/lark/output.rs)。
