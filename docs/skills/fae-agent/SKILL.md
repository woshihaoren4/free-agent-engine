---
name: "fae-agent"
description: "Creates, configures, modifies, runs, and troubleshoots FAE agents. Invoke for agent initialization, config, prompts, models, tools, skills, MCP, or sessions."
---

# FAE Agent

使用本 Skill 通过 `fae` 命令和配置文件创建、修改、运行或排查 FAE Agent。

## 核心原则

- 默认使用 `fae init` 创建 Agent，不编写代码。
- 使用 `<agent-id>_config.json` 管理模型、会话、工具、Skill 和 MCP。
- 使用 `<agent-id>_prompt.txt` 管理 Agent 的角色、行为和输出要求。
- 运行和验证统一使用 `fae agent`。
- 本 Skill 只处理 Agent 命令与配置，不展开底层实现。

## 标准流程

1. 确定 Agent ID、模型、职责、会话隔离要求和所需能力。
2. 使用 `fae init` 生成基础配置：

```bash
fae init --agent-id reviewer --model gpt-5
```

3. 编辑 `${FAE_HOST:-~/.fae}/agents/reviewer_config.json`，只保留 Agent 实际需要的 Tool、
   Skill 和 MCP。
4. 创建或修改 `${FAE_HOST:-~/.fae}/agents/reviewer_prompt.txt`，写清角色、边界、工作步骤和
   输出要求。
5. 使用 `fae agent` 启动并验证：

```bash
fae agent --agent-id reviewer
fae agent --agent-id reviewer "review this workspace"
```

6. 根据实际输出调整 config 或 prompt，再次用同一命令验证。

自定义 home：

```bash
fae --fae-home /path/to/fae-home init --agent-id reviewer --model gpt-5
fae --fae-home /path/to/fae-home agent --agent-id reviewer
```

## 文件约定

Agent ID 为 `reviewer` 时，默认加载：

```text
${FAE_HOST:-~/.fae}/agents/reviewer_config.json
${FAE_HOST:-~/.fae}/agents/reviewer_prompt.txt
```

Agent ID 必须是单个非空路径组件。配置中的 `agent.name` 必须与 Agent ID 一致。

`fae init`：

- 创建 Agent config 及所需的 home 子目录。
- 启用全部内置工具。
- 收集当前 `<FAE_HOST>/skills/*/SKILL.md` 对应的已安装 Skill。
- 已有 config 默认不会覆盖；使用 `--force` 才会替换。
- 不创建也不覆盖 prompt。自定义 Agent 必须准备对应的 prompt 文件。

## 最小配置

```json
{
  "agent": {
    "name": "reviewer",
    "user_id": "local",
    "session_id": "review-session",
    "metadata": {}
  },
  "model": {
    "model": "gpt-5",
    "trigger_compression_size": 32000,
    "history_turns": 20,
    "max_completion_tokens": 65536,
    "temperature": 0.2,
    "max_tool_iterations": 8
  },
  "tools": ["read_file"],
  "skills": [
    {
      "type": "name",
      "value": "fae-agent"
    }
  ],
  "mcp_servers": []
}
```

Prompt 文件只保存纯文本 system prompt，不使用 JSON，也不写入某一次用户请求。

## 修改策略

- 改模型或上下文：编辑 `model`。
- 改身份或输出风格：编辑 prompt，避免把行为规则散落到 config。
- 改会话隔离：修改 `agent.user_id` 或 `agent.session_id`。
- 增减内置工具：修改 `tools`，使用实际注册的工具名。
- 增减 Skill：修改 `skills`，按名称或路径配置。
- 增减 MCP：修改 `mcp_servers`，名称必须与 home 下 MCP 配置一致。
- 重新生成完整 config：

```bash
fae init --agent-id reviewer --model gpt-5 --force
```

`--force` 会重置 config 中的手工修改，但不会覆盖 prompt。使用前先确认确实需要重新生成。

## 按需阅读

- 配置字段、文件加载、命令参数和能力规则：
  [references/agent-api.md](references/agent-api.md)
- 创建、修改、复制、Skill、MCP、会话和排错配方：
  [references/recipes.md](references/recipes.md)

如果任务主要是创建多节点图、条件、并行、循环或父子流程，应改用 `fae-workflow` Skill。

## 排错顺序

1. 检查实际 `FAE_HOST` 或 `--fae-home`。
2. 检查 config 与 prompt 文件名是否匹配 Agent ID。
3. 检查 `agent.name`、模型名和数值字段是否合法。
4. 检查 Tool、Skill 和 MCP 名称是否真实存在。
5. 使用 `fae agent --agent-id <id> "reply with OK"` 做最小运行验证。
6. 根据错误定位配置加载、模型连接、能力路由或会话历史问题。
