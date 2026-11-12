# FAE Lark 网关配置与操作

使用 `fae lark` 将企业自建飞书机器人的消息交给本地 FAE Agent。
网关通过长连接接收消息，作为独立进程运行，通过同一 FAE home 的主服务执行 Agent。
普通 `fae`、`fae agent`、`fae workflow` 和 `fae server start` 不会自动启动网关。

## 1. 准备飞书应用

在飞书开放平台完成以下配置：

1. 创建企业自建应用，启用机器人能力并发布应用。
2. 选择长连接事件接收方式，订阅 `im.message.receive_v1`。
3. 开通权限：
   - `im:message.p2p_msg:readonly`
   - `im:message.group_at_msg:readonly`
   - `im:message:send_as_bot`
   - `im:message.reactions:write_only`
   - `cardkit:card:write`
4. 准备应用的 `app_id`、`app_secret` 及租户 `tenant_key`。
5. 群聊接入时将机器人加入群聊，使用飞书真实的 @机器人 mention。

身份使用发送者的飞书 `open_id`，不需要 `contact:user.employee_id:readonly` 权限。
消息缺少 `open_id` 时会报错，不会回退到员工 `user_id`、`union_id` 或 `master`。
网关忽略其他租户、机器人和不支持的聊天类型；目前只支持文本输入。
JSON 2.0 卡片需要飞书客户端 7.20 或更新版本。

## 2. 初始化与首次启动

先准备目标 Agent 的 config 和 prompt，按需验证本地模型及工具可用性。例如：

```bash
fae init --agent-id assistant --model gpt-5
# 创建 ~/.fae/agents/assistant_prompt.txt，并按需修改 assistant_config.json。
fae agent --agent-id assistant --session-id lark-setup "Reply with OK only."
```

初始化网关，指定 Agent 执行任务的工作区：

```bash
fae --workspace /absolute/path/to/workspace lark init
```

不传 `--workspace` 时，网关默认使用当前用户的 home 目录 `~`，而非命令执行目录。
初始化只生成 `<FAE_HOST>/lark.config`，不会启动网关或生成 Agent。
模板中的凭据是占位符，填写后才能校验、启动。

编辑 `lark.config`，将 `agent_id` 设为 `assistant`，填写飞书凭据，再执行：

```bash
fae lark check
fae lark check --online
fae lark show
fae lark start
fae lark status
```

`check` 校验配置及所引用的 Agent、子 Agent、类型化 Workflow 依赖；不调用模型，
不能替代实际 Agent 运行验证。`check --online` 额外验证机器人认证，不发送消息，
也不能证明事件订阅和所有消息/卡片权限均已生效。

启动后在飞书私聊发送文本，在群聊 @机器人发送文本，并确认回复。
真实飞书权限、长连接和网络恢复需要实际环境验证。

## 3. 文件与 Home

默认配置路径：

```text
${FAE_HOST:-~/.fae}/lark.config
```

相关文件：

```text
<FAE_HOST>/
├── agents/<agent-id>_config.json
├── agents/<agent-id>_prompt.txt
├── lark.config
├── lark/
│   ├── gateway.log
│   ├── runtime.json
│   ├── control.token
│   └── records/
├── requests/
├── memory/<open_id>.jsonl
└── session/<agent-id>/<open_id>/<thread_id>.jsonl
```

`runtime.json` 和控制 token 由程序管理；`records/` 保存接收及投递记录，
`requests/` 保存主服务的外部请求接受记录。不要通过删除这些记录来修复发送失败，
以免破坏恢复和去重依据。

初始化创建的 `lark.config` 在 Unix 上权限为 `0600`。不要把真实密钥写入共享文档或提交到仓库。
查看配置使用 `fae lark show`，其输出将 `app_secret` 隐去。

自定义 home 时，初始化、校验、启停与 Agent 配置使用同一目录：

```bash
fae --fae-home /path/to/fae-home --workspace /absolute/path/to/workspace lark init
fae --fae-home /path/to/fae-home lark check
fae --fae-home /path/to/fae-home lark start
fae --fae-home /path/to/fae-home lark status
fae --fae-home /path/to/fae-home lark stop
```

也可使用环境变量 `FAE_HOST`。`--workspace` 在 `lark init` 时写入配置；
已有配置以其中的 `workspace` 为准。对 `check`、`show`、`start` 显式传入不同的
`--workspace` 会报冲突，不能用它临时覆盖网关工作区。

## 4. 配置字段

以下为完整结构示例。凭据占位符必须替换，工作区必须是实际存在的目录：

```json
{
  "version": 1,
  "app_id": "<APP_ID>",
  "app_secret": "<APP_SECRET>",
  "tenant_key": "<TENANT_KEY>",
  "agent_id": "assistant",
  "user_id_whitelist": ["ou_alice", "ou_bob"],
  "workspace": "/absolute/path/to/workspace",
  "group": {
    "require_mention": true,
    "reply_in_thread": true
  },
  "execution": {
    "max_concurrent_rounds": 4,
    "session_idle_secs": 1800
  },
  "output": {
    "flush_interval_ms": 300
  },
  "storage": {
    "completed_record_retention_days": 7
  }
}
```

| 字段 | 默认值与规则 |
| --- | --- |
| `version` | 必填，当前只接受 `1` |
| `app_id` | 必填非空应用 ID；兼容旧字段名 `app_key` |
| `app_secret` | 必填非空应用密钥 |
| `tenant_key` | 必填非空租户标识，接收消息的租户必须一致 |
| `agent_id` | 必填，加载同一 home 下对应 Agent；初始化模板为 `fae` |
| `user_id_whitelist` | 省略或 `null` 允许所有用户；`[]` 拒绝所有用户；数组按发送者 `open_id` 精确匹配 |
| `workspace` | 省略为 `~`；展开后必须是存在的绝对目录，所有网关任务使用此工作区 |
| `group.require_mention` | 默认 `true`，必须为 `true` |
| `group.reply_in_thread` | 默认 `true`；旧值 `false` 会提示已弃用并归一为 `true` |
| `execution.max_concurrent_rounds` | 默认 `4`，范围 `1..8`，限制网关同时执行的新轮次 |
| `execution.session_idle_secs` | 默认 `1800`，范围 `1..86400`，空闲会话 actor 回收时间，单位秒 |
| `output.flush_interval_ms` | 默认 `300`，范围 `100..5000`，检查待投递输出的间隔，单位毫秒 |
| `storage.completed_record_retention_days` | 默认 `7`，范围 `1..365`，已完成接收记录的保留天数 |

配置对象拒绝未知字段。旧 `execution.session_queue_capacity` 仍可解析，但被忽略；
不要再配置队列容量。凭据和 `agent_id` 不能留为 `<...>` 占位符或包含换行。

白名单同样约束私聊、群聊、话题内外和管理命令。被拒绝的用户收到 `权限不足`，
不会开启 Agent 会话或执行命令。填写 `open_id`，不要用员工 `user_id` 替代。

## 5. 管理命令与配置生效

| 命令 | 操作 |
| --- | --- |
| `fae lark init` | 创建配置，已有配置时拒绝覆盖 |
| `fae lark init --force` | 替换网关配置，会丢失原有手工配置；确认需要重建后使用 |
| `fae lark check` | 离线校验配置及静态依赖 |
| `fae lark check --online` | 增加机器人认证验证 |
| `fae lark show` | 显示加载后的配置，隐去应用密钥 |
| `fae lark start` | 后台启动；若已运行则返回已有状态 |
| `fae lark start --foreground` | 前台运行，用于直接观察日志；不可与已有网关并行启动 |
| `fae lark status` | 查看状态，不启动网关 |
| `fae lark stop` | 停止网关，保留共享主服务 |
| `fae server status`、`fae server show` | 检查同一 home 的主服务及连接信息 |

`lark start` 自动发现或启动主服务，默认监听 `127.0.0.1:6666`。
修改 `lark.config` 后校验并重启网关：

```bash
fae lark check
fae lark stop
fae lark start
fae lark status
```

已有 actor 的 Agent 配置会被冻结；修改 Agent config 或 prompt 后，
需要在话题外开启新会话验证新配置，不能假定旧话题立即重新加载。

需要完全停止网关和主服务时，依次执行：

```bash
fae lark stop
fae server stop
```

停止网关会取消未完成的网关任务；停止主服务还会影响共享它的终端、Workflow 和其他任务。
修改主服务所需的模型、搜索等环境变量后，主服务也需重启以读取新值。

## 6. 飞书消息与话题会话

- 话题外的普通消息先创建话题，再执行 Agent；真实飞书 `thread_id` 即 session ID。
  无法取得真实话题 ID 时不执行 Agent。
- 在同一话题回复会继续该话题的会话，并按发送者 `open_id` 隔离。
  群聊话题内外的普通消息和命令都必须 @机器人。
- 发送者 `open_id` 作为 Agent 的 `user_id`；网关映射覆盖 Agent config 中的用户和会话 ID。
  用户长期记忆跨该用户的会话共享，todo 按计划隔离。
- 旧聊天会话映射仅用于恢复；新消息不会复用旧聊天选择文件，也不会自动迁移旧身份数据。

话题空闲时支持：

| 飞书命令 | 行为 |
| --- | --- |
| `/help`、`/session -h`、`/session --help` | 显示帮助 |
| `/status`、`/session` | 查看当前话题 session、模型和状态 |
| `/session clean`、`/clean` | 清理当前话题历史和 todos，保留话题绑定及用户记忆 |
| `/todo <message>` | 规划并执行任务，目标 Agent 必须配置 `todo` 工具 |
| `/steer <message>` | 补充正在执行的轮次；空闲时失败 |
| `/session new` | 提示在话题外发送消息开始新会话 |
| `/session id=<id>` | 提示在对应话题回复，不直接切换会话 |

话题外的管理命令不会操作之前的会话；`/help` 可显示帮助，
`/todo <message>` 可创建话题并启动任务。

会话执行中，后续输入自动成为当前轮次的补充，包括 `/clean`、`/status` 等命令文本；
会话切换命令仍只返回话题操作提示。需要实际清理或查询时，等任务完成再发送命令。
同一会话没有等待执行的消息队列；达到并发上限时拒绝新轮次并提示稍后重试。
给正在执行的轮次补充输入不占用新的并发名额。

新话题使用含 `Typing` 表情的回复显示接收状态，不额外添加 reaction；
话题内的后续输入使用 `Typing` reaction，最终回复投递后移除。
网关只投递完整轮次结束后的最终答案，隐藏中间模型输出、工具调用、推理和子 Agent 内部消息。
长回复分页；错误带 `ERROR`；卡片发送失败时可降级为文本并提示。

## 7. 状态、日志与排错

优先执行以下只读检查：

```bash
fae lark show
fae lark check
fae lark check --online
fae lark status
fae server status
fae server show
```

后台日志位于 `<FAE_HOST>/lark/gateway.log`。状态包含 `runtime.ready`、
`service_ready`、`active`、`undelivered` 和 `last_error`。
`queued` 是尚处于接收状态的持久化记录数，不代表存在等待执行队列。

| 现象 | 检查与处理 |
| --- | --- |
| 配置校验失败 | 检查 JSON、`version`、占位符、未知字段、数值范围、Agent config/prompt 和引用依赖 |
| `--workspace conflicts with lark.config workspace` | 修改配置中的工作区并重启；不要通过启动参数覆盖 |
| 认证失败 | 检查应用 ID/密钥、应用发布状态及网络；使用 `check --online` 定位 |
| 私聊或群聊无回复 | 检查事件订阅、长连接、权限、租户、文本类型、群成员资格及真实 @mention |
| `权限不足` | 检查白名单是否误设为空、是否填写发送者 `open_id`；修改后重启 |
| 缺少 `open_id` | 检查飞书事件发送者身份；不使用其他 ID 作为回退 |
| 无法创建话题 | 检查消息/话题权限及飞书返回的真实 thread ID；不能用 message ID 代替 |
| `runtime.ready=false` 或启动超时 | 检查主服务状态、长连接和后台日志；前台运行可观察具体错误 |
| 当前执行数已达上限 | 等待后重试，或在允许范围内调整并发数并重启 |
| `undelivered` 非零或卡片降级 | 检查发送、reaction、CardKit 权限及网络，结合 `last_error` 和日志判断 |
| `unsupported_version` | 升级后重启旧主服务；若旧协议无法处理停止命令，核实 PID 后再处理旧进程 |

网关保留未投递结果并在恢复后重试投递。主服务重启、提交状态不确定或事件重放缺口时，
不会盲目重跑已可能产生副作用的工具；先检查记录与实际结果，再决定是否重新提交任务。
接收记录保留期和空闲 actor 回收不会清除持久化话题绑定。
工作区和会话隔离不构成文件系统沙箱，实际访问能力仍由 Agent 工具与运行环境决定。
