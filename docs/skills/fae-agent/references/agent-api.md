# FAE Agent API

## 1. 核心类型

| 类型 | 职责 |
| --- | --- |
| `SingleAgentInfo` | Agent 名称、用户、会话 ID 和 metadata |
| `SingleAgentModelConfig` | 模型、上下文、历史和工具迭代限制 |
| `SingleAgentConfig` | JSON 中的完整静态配置 |
| `SingleAgentSource` | 通过 agent ID 或显式路径定位配置与 prompt |
| `SingleAgentEnv` | 一次执行的配置来源、输入和 session |
| `SingleAgentPlanBuilder` | 加载配置并构造 `SingleAgentPlan` |
| `SingleAgentSession` | 提交后续输入并接收流式事件 |

## 2. 配置来源

### Agent ID

```rust
let (env, session) =
    SingleAgentEnv::from_agent_id("reviewer", "Review Cargo.toml");
```

默认路径：

```text
${FAE_HOST:-~/.fae}/agents/reviewer_config.json
${FAE_HOST:-~/.fae}/agents/reviewer_prompt.txt
```

agent ID 必须是单个非空路径组件，不能使用 `../reviewer`、`team/reviewer` 等路径。配置中的
`agent.name` 必须等于请求的 agent ID。

### 显式路径

```rust
let (env, session) = SingleAgentEnv::from_paths(
    "/opt/fae/reviewer.json",
    "/opt/fae/reviewer.txt",
    "Review Cargo.toml",
);
```

显式路径不要求文件名后缀，也不要求 `agent.name` 与文件名一致。库接口不会展开 `~`；调用方应传入
已经解析的路径。

通用构造方式：

```rust
let source = SingleAgentSource::Paths {
    config: config_path,
    prompt: prompt_path,
};
let (env, session) = SingleAgentEnv::new(source, input);
```

## 3. JSON 配置

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
    "model": "gpt-xxx",
    "context_size": 32000,
    "history_turns": 20,
    "max_completion_tokens": 4096,
    "temperature": 0.2,
    "max_tool_iterations": 8
  },
  "tools": ["read_file", "execute_command"],
  "skills": [
    { "type": "name", "value": "weather" },
    { "type": "path", "value": "/opt/fae/skills/review/SKILL.md" }
  ],
  "mcp_servers": ["maps"]
}
```

字段规则：

| 字段 | 规则 |
| --- | --- |
| `agent.name` | 非空；按 ID 加载时必须与 ID 相同 |
| `agent.user_id` | 非空；用于会话历史路径 |
| `agent.session_id` | 非空；用于会话历史路径 |
| `agent.metadata` | 可省略，默认为空对象 |
| `model.model` | 非空，必须可被 `ModelRuntime` 使用 |
| `model.context_size` | 大于 0 |
| `model.history_turns` | 可为 0；每轮按 user/assistant 两条消息读取 |
| `model.max_completion_tokens` | 可省略或为 `null` |
| `model.temperature` | 可省略或为 `null` |
| `model.max_tool_iterations` | 可省略，默认 8；显式值必须大于 0 |
| `tools` | 可省略，默认为空数组 |
| `skills` | 可省略，默认为空数组 |
| `mcp_servers` | 可省略，默认为空数组 |

system prompt 必须保存在独立文本文件中。Builder 会在 prompt 后附加已解析 Skill 的名称、描述与
`SKILL.md` 路径。

## 4. Builder

默认 home：

```rust
let builder = SingleAgentPlanBuilder::new();
```

测试或嵌入场景使用隔离目录：

```rust
let builder = SingleAgentPlanBuilder::with_home_dir(temp_dir);
assert_eq!(builder.home_dir(), temp_dir.as_ref());
```

预检配置：

```rust
let source = SingleAgentSource::AgentId("reviewer".into());
let (config, prompt) = builder.load_config(&source).await?;
```

`load_config` 会读取并反序列化文件、校验必填字段和数值，并检查 ID 与 `agent.name`。Tool、
Skill 和 MCP 是否真实存在，要到 plan 构建阶段通过对应 runtime 查询后才能确定。

注册自定义 builder：

```rust
let mut engine = EngineBuilder::new();
engine.add_runtime(PlanRuntime::new());
engine.add_runtime(ModelRuntime::new());
engine.add_runtime(SessionRuntime::with_host_dir(&home));
engine.add_plan_builder(SingleAgentPlanBuilder::with_home_dir(&home));
```

同一个 engine 中所有依赖磁盘的 runtime 应使用同一个 home，避免 agent 配置、会话和 Skill
来自不同目录。

## 5. Runtime 依赖

| 配置能力 | 必需组件 |
| --- | --- |
| 基础模型调用 | `PlanRuntime`、`ModelRuntime`、`SessionRuntime`、`SingleAgentPlanBuilder` |
| `tools` 非空 | 能按工具名查询和执行的 `ToolsRuntime` |
| `skills` 非空 | `SkillRuntime` |
| `mcp_servers` 非空 | `McpRuntime` |

`Engine::default().await` 已注册上述默认组件、`DefaultTools` 和 SingleAgent builder。

自定义 engine 示例：

```rust
let mut builder = EngineBuilder::new();
builder.add_runtime(PlanRuntime::new());
builder.add_runtime(ModelRuntime::new());
builder.add_runtime(SessionRuntime::new());
builder.add_runtime(SkillRuntime::new());
builder.add_runtime(McpRuntime::new());

let mut tools = ToolsRuntime::new();
tools.add_tool(Box::new(DefaultTools::default()));
builder.add_runtime(tools);

builder.add_plan_builder(SingleAgentPlanBuilder::new());
let engine = builder.build().await;
```

## 6. Tool、Skill 与 MCP 路由

### Tool

配置中的每个 `tools` 名称会通过 `TaskType::Tool` 查询函数定义。模型返回的函数名会映射回配置的
runtime 工具名。多个工具不能暴露相同的模型函数名。

### Skill

`SkillQuery` JSON 使用 tagged 结构：

```json
{ "type": "name", "value": "weather" }
```

名称查询默认定位到 `${FAE_HOST:-~/.fae}/skills/<name>/SKILL.md`。路径查询可指向一个
`SKILL.md` 或包含多个 Skill 的目录。

Skill 不会自动变成函数工具。Builder 只将 Skill metadata 和路径加入 system prompt，由模型按需
读取和遵循。

### MCP

`mcp_servers` 中的名称由 `McpRuntime` 从 FAE home 的 `mcp` 目录查找。每个 MCP 工具向模型暴露
为 `<server>__<tool_name>`，避免不同 server 的普通工具名冲突。该名称仍不能与普通 Tool 暴露的
函数名重复。

## 7. 会话与多轮

首次执行：

```rust
let (env, session) = SingleAgentEnv::from_agent_id("reviewer", first_input);
let execution = engine.launch(env).await?;
consume_turn(&session).await?;
execution.result::<()>().await?;
```

后续轮次复用同一个 session：

```rust
session.call("Now propose a fix.".to_string()).await?;
consume_turn(&session).await?;
```

注意：

- session 在首次 plan 构建时绑定 engine；绑定前调用 `call` 会失败。
- 活跃轮次仍接受输入时，新输入会进入该轮的 pending queue。
- 当前轮已进入收尾阶段时，`call` 会等待其结束，再启动新轮。
- `user_id` 与 `session_id` 对应持久化文件
  `${FAE_HOST:-~/.fae}/memory/<user_id>/session/<session_id>.jsonl`。
- 每轮完成后保存 user 与 assistant 消息。

## 8. 事件

`SessionEvent` 包含 `turn_id`、`source` 和 `SessionEventData`。常见事件：

| 事件 | 含义 |
| --- | --- |
| `TurnStarted` | 新一轮开始 |
| `UserInput` | 活跃轮次吸收了追加输入 |
| `ModelReasoning` | 流式 reasoning 增量 |
| `ModelOutput` | 流式正文增量 |
| `ToolCall` | 模型发起 Tool 或 MCP 调用 |
| `ToolOutput` | 工具流式或最终输出 |
| `Completed` | 当前轮完成 |
| `Failed` | 当前轮失败 |

独立 SingleAgent 中，`Completed` 和 `Failed` 是终止事件，可使用 `event.is_terminal()` 判断。
嵌入 workflow 后事件还会带 `workflow_id` 与 `node_id`；此时 Agent 的轮次结束不等于整个
workflow 结束。

## 9. Workflow 中使用

```rust
WorkflowAction::SingleAgent {
    source: SingleAgentSource::AgentId("reviewer".into()),
    input: json!("Review: {$input.content}"),
}
```

Workflow 会创建绑定到父 `WorkflowSession` 的 child session，并转发 Agent 事件。引擎必须同时
注册 `WorkflowPlanBuilder` 与 `SingleAgentPlanBuilder`，且其 home 配置应保持一致。
