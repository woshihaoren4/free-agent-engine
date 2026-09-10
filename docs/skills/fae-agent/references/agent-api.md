# FAE Agent 配置参考

本文说明如何使用 `fae` 命令和磁盘配置创建、修改及运行 Agent。

## 1. 创建 Agent

创建默认 `fae` Agent：

```bash
fae init
```

指定 Agent ID 和模型：

```bash
fae init --agent-id reviewer --model gpt-5
```

在自定义 home 中创建：

```bash
fae --fae-home /path/to/fae-home init --agent-id reviewer --model gpt-5
```

`--model` 省略时优先读取 `FAE_DEFAULT_MODEL`，否则默认使用 `gpt-4o-mini`。

若 config 已存在，`fae init` 会拒绝覆盖。确认需要重建后使用：

```bash
fae init --agent-id reviewer --model gpt-5 --force
```

`--force` 只替换 `<agent-id>_config.json`，不会创建或覆盖 `<agent-id>_prompt.txt`。

## 2. 文件位置

Agent ID 为 `reviewer` 时：

```text
${FAE_HOST:-~/.fae}/agents/reviewer_config.json
${FAE_HOST:-~/.fae}/agents/reviewer_prompt.txt
```

相关目录：

```text
${FAE_HOST:-~/.fae}/
├── agents/
├── memory/
├── mcp/
├── skills/
└── workflows/
```

规则：

- Agent ID 必须是单个非空路径组件，不能包含 `/`、`\` 或 `..`。
- 按 ID 加载时，config 中的 `agent.name` 必须等于 Agent ID。
- config 必须是合法 JSON。
- prompt 必须是纯文本文件。

## 3. Agent Config

完整结构：

```json
{
  "agent": {
    "name": "reviewer",
    "user_id": "alice",
    "session_id": "release-review",
    "metadata": {
      "team": "platform"
    }
  },
  "model": {
    "model": "gpt-5",
    "context_size": 32000,
    "history_turns": 20,
    "max_completion_tokens": 65536,
    "temperature": 0.2,
    "max_tool_iterations": 8
  },
  "tools": ["read_file", "execute_command"],
  "skills": [
    {
      "type": "name",
      "value": "fae-agent"
    },
    {
      "type": "path",
      "value": "/workspace/skills/reviewer/SKILL.md"
    }
  ],
  "mcp_servers": ["maps"]
}
```

### Agent 字段

| 字段 | 规则 |
| --- | --- |
| `agent.name` | 非空；按 ID 运行时必须与 Agent ID 相同 |
| `agent.user_id` | 非空；参与确定会话历史路径 |
| `agent.session_id` | 非空；参与确定会话历史路径 |
| `agent.metadata` | 可省略，默认为空对象；用于保存业务标签 |

修改 `user_id` 或 `session_id` 会切换到另一份持久化历史。需要保持上下文时，不要随意更改。

### Model 字段

| 字段 | 规则 |
| --- | --- |
| `model.model` | 非空；必须是当前模型服务可用的模型名 |
| `model.context_size` | 可省略，默认 `32000`；显式值必须大于 0，超限时先压缩上下文 |
| `model.history_turns` | 可为 0；控制读取多少轮历史 |
| `model.max_completion_tokens` | 可省略，默认 `65536`；设为 `null` 时不向模型传递该限制 |
| `model.temperature` | 可省略或为 `null` |
| `model.max_tool_iterations` | 可省略，默认 8；显式值必须大于 0 |

`max_tool_iterations` 用于限制单轮连续工具调用，避免 Agent 无限循环。只有确认任务确实需要更多
步骤时才提高。

上下文压缩通过内置 `workflow.compression` runtime 调用同一个模型完成。压缩结果会以
`summary` 角色追加到 session JSONL；后续加载历史时从最新的 `summary` 开始，更早的消息不再
加入模型上下文。

### 可选能力字段

| 字段 | 含义 | 省略值 |
| --- | --- | --- |
| `tools` | Agent 可直接调用的内置工具 | `[]` |
| `skills` | 注入到 Agent 上下文的 Skill | `[]` |
| `mcp_servers` | Agent 可访问的 MCP server | `[]` |

## 4. System Prompt

`reviewer_prompt.txt` 只保存 Agent 的长期行为约束，例如：

```text
You are a code reviewer.

Focus on correctness, regressions, security, and missing tests.
Inspect relevant files before reaching conclusions.
Report findings by severity and include precise file references.
Keep the final summary concise.
```

建议包含：

- Agent 的角色和主要目标。
- 必须执行的步骤和质量标准。
- 禁止事项和权限边界。
- 输出语言、结构和详略要求。
- 使用 Tool、Skill 或 MCP 的判断原则。

不要包含：

- 某一次任务的具体输入。
- 密钥、token 或其他敏感信息。
- 与 config 重复的模型名、会话 ID 或能力列表。
- 无法通过当前 Tool、Skill 或 MCP 实现的承诺。

修改 prompt 后重新启动 `fae agent`，新会话运行会读取最新内容。

## 5. 内置 Tool

`fae init` 默认启用：

```json
[
  "execute_command",
  "read_file",
  "write_file",
  "list_directory",
  "apply_patch",
  "send_http_request",
  "execute_python"
]
```

按最小权限原则删除不需要的工具。例如只读审查 Agent 可保留：

```json
{
  "tools": ["read_file", "list_directory"]
}
```

工具名必须与 `fae` 注册名称完全一致。模型是否调用工具还取决于模型能力、prompt 和具体任务。

## 6. Skill

按名称加载已安装 Skill：

```json
{
  "skills": [
    {
      "type": "name",
      "value": "fae-agent"
    }
  ]
}
```

对应默认路径：

```text
${FAE_HOST:-~/.fae}/skills/fae-agent/SKILL.md
```

按路径加载：

```json
{
  "skills": [
    {
      "type": "path",
      "value": "/workspace/skills/reviewer/SKILL.md"
    }
  ]
}
```

`fae init` 只收集初始化当时已经安装在 `<home>/skills/` 下的 Skill。之后新增或删除 Skill 时，
手工同步 config，或确认可接受重置其他配置后使用 `fae init --force` 重新生成。

Skill 提供工作指引，不会自动赋予工具能力。Skill 依赖的工具仍需出现在 `tools` 或
`mcp_servers` 中。

## 7. MCP

Agent config 通过 server 名称启用 MCP：

```json
{
  "mcp_servers": ["maps"]
}
```

名称必须与 `${FAE_HOST:-~/.fae}/mcp/` 下 MCP 配置中的 `mcpServers` key 一致：

```json
{
  "mcpServers": {
    "maps": {
      "command": "maps-mcp-server",
      "args": [],
      "env": {}
    }
  }
}
```

远程 MCP 示例：

```json
{
  "mcpServers": {
    "maps": {
      "url": "https://example.test/mcp",
      "headers": {
        "Authorization": "Bearer ${MAPS_TOKEN}"
      }
    }
  }
}
```

不要把真实 token 写入共享配置或提交到仓库。Agent 可见的 MCP 工具名通常为
`<server>__<tool>`。

## 8. 运行 Agent

交互运行：

```bash
fae agent --agent-id reviewer
```

直接提交第一条消息：

```bash
fae agent --agent-id reviewer "review this workspace"
```

`fae` 不带子命令时等价于运行默认 `fae` Agent：

```bash
fae
fae "summarize the current workspace"
```

使用显式 config 和 prompt：

```bash
fae agent \
  --agent-config /path/to/reviewer_config.json \
  --agent-prompt /path/to/reviewer_prompt.txt \
  "review this workspace"
```

两项显式路径必须同时提供。保留终端滚动记录：

```bash
fae --no-alt-screen agent --agent-id reviewer
```

禁用颜色：

```bash
fae --color never agent --agent-id reviewer
```

会话内命令：

| 命令 | 用途 |
| --- | --- |
| `/help` | 显示命令 |
| `/status` | 显示模型和 session |
| `/clear` | 清空当前界面内容 |
| `/exit`、`/quit` | 退出 |

`/clear` 只清空界面，不删除磁盘会话历史。

## 9. 会话历史

历史默认保存在：

```text
${FAE_HOST:-~/.fae}/memory/<user_id>/session/<session_id>.jsonl
```

`history_turns` 控制新一轮读取的历史轮数。排查上下文问题时检查：

- `user_id` 和 `session_id` 是否仍是预期值。
- 是否切换了 `FAE_HOST` 或 `--fae-home`。
- `history_turns` 是否为 0 或过小。
- 历史文件是否存在且属于当前 Agent。

不同用途的 Agent 应使用不同 `session_id`，避免不相关上下文互相污染。

## 10. 验证配置

当前 `fae` 没有独立的 Agent config validate 子命令。使用最小请求完成加载和连接验证：

```bash
fae agent --agent-id reviewer "Reply with OK only."
```

需要验证 Tool：

```bash
fae agent --agent-id reviewer "List the current directory using an available tool."
```

需要验证 Skill：

```bash
fae agent --agent-id reviewer "List the skills available to you and use fae-agent guidance."
```

需要验证 MCP：

```bash
fae agent --agent-id reviewer "List the tools available from the maps MCP server."
```

验证请求应最小且无破坏性。确认读取能力后，再测试写文件、执行命令或外部请求。
