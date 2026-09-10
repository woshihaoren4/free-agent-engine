# FAE Agent 配置配方

以下配方只使用 `fae` 命令和 Agent 配置文件。

## 1. 创建默认 Agent

初始化：

```bash
fae init
```

确保 `~/.fae/agents/fae_prompt.txt` 存在，然后启动：

```bash
fae
```

直接提交任务：

```bash
fae "summarize the current workspace"
```

`fae init` 会创建 `fae_config.json`，但不会创建或覆盖 prompt。安装程序通常会提供默认
`fae_prompt.txt`；若不存在，需要手工创建。

## 2. 创建专用审查 Agent

生成基础配置：

```bash
fae init --agent-id reviewer --model gpt-5
```

创建 `~/.fae/agents/reviewer_prompt.txt`：

```text
You are a focused code reviewer.

Inspect relevant files before reporting findings.
Prioritize correctness, regressions, security, and missing tests.
Report findings by severity with precise file references.
Do not modify files unless the user explicitly asks for fixes.
```

将 `reviewer_config.json` 中的能力缩减为只读：

```json
{
  "tools": ["read_file", "list_directory"],
  "skills": [
    {
      "type": "name",
      "value": "fae-agent"
    }
  ],
  "mcp_servers": []
}
```

运行：

```bash
fae agent --agent-id reviewer "review the current workspace"
```

## 3. 创建可修改文件的实现 Agent

初始化：

```bash
fae init --agent-id implementer --model gpt-5
```

在 `implementer_config.json` 中保留：

```json
{
  "tools": [
    "execute_command",
    "read_file",
    "write_file",
    "list_directory",
    "apply_patch"
  ]
}
```

在 `implementer_prompt.txt` 中明确：

```text
Inspect the repository before editing.
Keep changes scoped to the request.
Use existing project conventions.
Verify the result with the project's standard checks.
Never discard unrelated user changes.
```

启动：

```bash
fae agent --agent-id implementer
```

## 4. 修改模型参数

直接编辑 `<agent-id>_config.json` 的 `model`：

```json
{
  "model": {
    "model": "gpt-5",
    "context_size": 64000,
    "history_turns": 10,
    "max_completion_tokens": 8192,
    "temperature": 0.1,
    "max_tool_iterations": 12
  }
}
```

验证：

```bash
fae agent --agent-id reviewer "Reply with your role in one sentence."
```

若只想更换模型且接受重置整个 config，可使用：

```bash
fae init --agent-id reviewer --model gpt-5 --force
```

`--force` 会重新启用全部内置工具、重新收集已安装 Skill，并重置其他 config 字段。prompt 保持
不变。

## 5. 添加或删除 Skill

按名称添加：

```json
{
  "skills": [
    {
      "type": "name",
      "value": "fae-agent"
    },
    {
      "type": "name",
      "value": "fae-workflow"
    }
  ]
}
```

按路径添加：

```json
{
  "skills": [
    {
      "type": "path",
      "value": "/workspace/docs/skills/domain-review/SKILL.md"
    }
  ]
}
```

修改后运行：

```bash
fae agent --agent-id reviewer "List the skills available to you."
```

若按名称加载失败，确认 Skill 位于
`${FAE_HOST:-~/.fae}/skills/<skill-name>/SKILL.md`。删除 Skill 时同时删除 config 中对应项。

## 6. 添加 MCP

先在 `${FAE_HOST:-~/.fae}/mcp/` 下准备包含 `mcpServers` 的 MCP 配置，再把 server 名加入
Agent config：

```json
{
  "mcp_servers": ["maps", "issue-tracker"]
}
```

启动并验证：

```bash
fae agent --agent-id reviewer "List the MCP tools available to you."
```

若启动失败，检查 Agent config 中的名称是否与 MCP 配置的 `mcpServers` key 完全一致。

## 7. 为不同任务隔离会话

日常会话：

```json
{
  "agent": {
    "name": "assistant",
    "user_id": "local",
    "session_id": "daily",
    "metadata": {}
  }
}
```

发布审查会话：

```json
{
  "agent": {
    "name": "release-reviewer",
    "user_id": "local",
    "session_id": "release-review",
    "metadata": {}
  }
}
```

分别运行：

```bash
fae agent --agent-id assistant
fae agent --agent-id release-reviewer
```

不要让用途无关的 Agent 共享 `session_id`。需要无历史上下文时，将 `history_turns` 设为 `0`。

## 8. 使用自定义 FAE Home

创建隔离 Agent：

```bash
fae --fae-home /tmp/fae-review init --agent-id reviewer --model gpt-5
```

在 `/tmp/fae-review/agents/` 下准备 `reviewer_prompt.txt` 并修改 config，然后运行：

```bash
fae --fae-home /tmp/fae-review agent --agent-id reviewer
```

`--fae-home` 同时影响 Agent、Skill、MCP 和 session 的加载位置。创建和运行必须使用同一目录。

## 9. 使用显式 Config 和 Prompt

不按 Agent ID 加载时，同时传入两个文件：

```bash
fae agent \
  --agent-config /workspace/config/reviewer.json \
  --agent-prompt /workspace/config/reviewer.txt \
  "review this workspace"
```

适合临时实验或配置文件不在 FAE home 的场景。只传其中一个参数会被拒绝。

## 10. 调整 Prompt

当 Agent 能运行但行为不符合预期时，优先调整 prompt：

- 回答过长：规定输出结构和最大条目数。
- 未检查文件：要求先读取证据，再给结论。
- 随意修改：要求仅在用户明确授权后编辑。
- 不使用工具：说明哪些场景必须调用哪个能力。
- 输出不稳定：规定固定标题、字段或排序。

每次只改一个行为约束，并用相同请求复测：

```bash
fae agent --agent-id reviewer "review the current workspace"
```

不要通过堆叠重复、冲突或空泛的规则修正 prompt。

## 11. 最小验证顺序

先验证配置和模型：

```bash
fae agent --agent-id reviewer "Reply with OK only."
```

再验证只读工具：

```bash
fae agent --agent-id reviewer "List the current directory using a tool."
```

再验证 Skill：

```bash
fae agent --agent-id reviewer "List the skills available to you."
```

最后验证 MCP：

```bash
fae agent --agent-id reviewer "List the MCP tools available to you."
```

逐层验证能快速区分 config、模型、Tool、Skill 和 MCP 问题。

## 12. 排错清单

### 找不到 Config 或 Prompt

- 检查 `FAE_HOST` 或 `--fae-home`。
- 检查文件名是否为 `<agent-id>_config.json` 和 `<agent-id>_prompt.txt`。
- 检查两个文件是否都位于 `<home>/agents/`。
- 显式路径模式下，检查两个参数是否同时提供。

### Config 解析失败

- 检查 JSON 逗号、引号和括号。
- 检查是否保留 `agent` 和 `model` 两个完整对象。
- 检查 `skills` 是否使用 `{"type","value"}`，而不是纯字符串。
- 检查 `agent.name` 是否与运行时 Agent ID 相同。
- 检查 `context_size` 和 `max_tool_iterations` 是否大于 0。

### 模型不可用

- 检查 `model.model` 是否是当前服务支持的名称。
- 检查模型服务所需环境变量和凭据。
- 使用最小请求排除 prompt 或工具调用影响。

### Tool 不可用或未被调用

- 检查工具是否出现在 `tools`。
- 检查工具名是否拼写正确。
- 检查模型是否支持工具调用。
- 在 prompt 中明确该任务何时必须使用工具。
- 检查是否达到 `max_tool_iterations`。

### Skill 不可用

- 名称模式下检查 `<home>/skills/<name>/SKILL.md`。
- 路径模式下检查路径是否指向 `SKILL.md` 或 Skill 目录。
- 检查 Skill 依赖的 Tool 或 MCP 是否也已配置。

### MCP 不可用

- 检查 `mcp_servers` 名称和 MCP 配置 key。
- 检查本地命令或远程 URL 是否可用。
- 检查认证配置，但不要在日志或共享文件中暴露 token。

### 历史不符合预期

- 检查 `agent.user_id` 和 `agent.session_id`。
- 检查 `history_turns` 是否为 0 或过小。
- 检查运行时是否使用了不同的 `--fae-home`。
- `/clear` 只清空界面，不会删除持久化历史。
