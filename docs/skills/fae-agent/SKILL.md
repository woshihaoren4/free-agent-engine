---
name: "fae-agent"
description: "Builds, configures, runs, and troubleshoots FAE single agents. Invoke for agent config, prompts, sessions, tools, skills, MCP, or model execution."
---

# FAE Agent

使用本 Skill 在当前仓库中创建、配置、运行或排查 FAE SingleAgent。

## 适用范围

在以下任务中调用本 Skill：

- 新建或修改 `SingleAgentConfig`、agent JSON 或 system prompt。
- 使用 agent ID 或显式文件路径创建 `SingleAgentEnv`。
- 配置 `SingleAgentPlanBuilder`、模型、会话、Tool、Skill 或 MCP runtime。
- 实现单轮调用、多轮会话、流式事件消费或 workflow 中的 SingleAgent action。
- 排查配置加载、模型请求、历史记录、工具路由或会话状态问题。

如果任务主要是构建多节点图、条件、并行、循环或父子流程，应改用 `fae-workflow` Skill；
只有其中的节点需要调用 Agent 时，再同时参考本 Skill。

## 执行步骤

1. 阅读目标代码和相邻示例，确认当前 API。SingleAgent 配置和加载协议仍在演进，不要凭旧接口实现。
2. 确定配置来源：
   - 稳定部署使用 agent ID。
   - 测试、临时配置或外部目录使用显式 config/prompt 路径。
3. 将静态能力写入 `SingleAgentConfig`，将 system prompt 单独保存为文本文件。不要在
   `SingleAgentEnv` 中重复模型、工具或 prompt 配置。
4. 按配置能力注册 runtime。基础执行至少需要 `PlanRuntime`、`ModelRuntime`、
   `SessionRuntime` 和 `SingleAgentPlanBuilder`。
5. 使用 `SingleAgentEnv::from_agent_id` 或 `SingleAgentEnv::from_paths` 创建 ENV，并保留同时
   返回的 `SingleAgentSession`。
6. 需要实时输出时使用 `engine.launch`，持续消费 session 事件，再等待 execution result。
7. 多轮对话复用同一个已绑定 session，通过 `session.call(...)` 提交后续输入。
8. 添加与改动风险匹配的测试，并运行格式化、目标测试和 workspace 检查。

## 开始前按需阅读

- 配置格式、加载规则、类型、runtime 和事件契约：
  [references/agent-api.md](references/agent-api.md)
- 单轮、多轮、自定义 home、Tool/Skill/MCP 和排错配方：
  [references/recipes.md](references/recipes.md)

若任务涉及 Tool、Skill、MCP、自定义模型 client 或 workflow 嵌套，必须先阅读对应参考章节。

## 文件约定

使用 agent ID `reviewer` 时，默认加载：

```text
${FAE_HOST:-~/.fae}/agents/reviewer_config.json
${FAE_HOST:-~/.fae}/agents/reviewer_prompt.txt
```

`reviewer_config.json`：

```json
{
  "agent": {
    "name": "reviewer",
    "user_id": "local",
    "session_id": "review-session",
    "metadata": {}
  },
  "model": {
    "model": "gpt-xxx",
    "context_size": 32000,
    "history_turns": 20,
    "max_completion_tokens": 4096,
    "temperature": 0.2,
    "max_tool_iterations": 8
  },
  "tools": ["read_file"],
  "skills": [
    { "type": "name", "value": "weather" }
  ],
  "mcp_servers": []
}
```

`reviewer_prompt.txt` 只存放 system prompt，不要包 JSON，不要把用户本轮输入写入其中。

## 最小运行模式

```rust
use fae_agent::{Session, SingleAgentEnv};

async fn run() -> anyhow::Result<()> {
    let engine = fae_engine::Engine::default().await;
    let (env, session) =
        SingleAgentEnv::from_agent_id("reviewer", "Review the current workspace.");
    let execution = engine.launch(env).await?;

    while let Some(event) = session.answer().await? {
        if event.is_terminal() {
            break;
        }
    }

    execution.result::<()>().await?;
    engine.exit().await?;
    Ok(())
}
```

关键点：

- ENV 只描述配置来源和本轮输入；静态配置来自 JSON 与 prompt 文件。
- `agent.name` 必须与 agent ID 一致；agent ID 必须是单个非空路径组件。
- `Engine::default()` 已注册 SingleAgent 所需的默认 runtime 和 builder。
- 自定义 `EngineBuilder` 时必须显式注册依赖；配置中声明能力但漏注册 runtime 会在构建 plan
  或执行时失败。
- session 事件必须被持续消费；不要把 `execution.result()` 当作流式输出接口。

## 配置边界

- `context_size` 必须大于 0。
- `max_tool_iterations` 必须大于 0，用于阻止模型无限调用工具。
- `history_turns` 表示读取的历史轮数，内部按一轮两条消息计算。
- `max_completion_tokens` 和 `temperature` 可为 `null`。
- `tools` 使用已注册的工具路由名。
- `skills` 使用带 `type`/`value` 的 `SkillQuery` JSON。
- `mcp_servers` 使用 MCP 配置中的 server 名称；模型侧工具名会变为
  `<server>__<tool>`。
- `user_id` 与 `session_id` 同时决定持久化历史路径，二者都必须是安全的单路径段。

## 验证

至少运行：

```bash
cargo fmt --check
cargo test -p fae-agent single_agent
cargo test -p fae-engine
cargo check --workspace --all-targets
```

若修改 CLI 或示例，再运行：

```bash
cargo test -p fae
cargo check -p examples --all-targets
```

## 当前源码入口

- `crates/fae-agent/src/plan/single_agent.rs`
- `crates/fae-agent/src/session/mod.rs`
- `crates/fae-agent/src/skill.rs`
- `crates/fae-agent/src/mcp.rs`
- `crates/fae-engine/src/engine_rt/model_runtime.rs`
- `crates/fae-engine/src/engine_rt/session_runtime.rs`
- `crates/fae-engine/src/engine_rt/skill_runtime.rs`
- `crates/fae-engine/src/engine_rt/mcp_runtime.rs`
- `crates/fae-engine/src/lib.rs`
- `examples/single_agent.rs`
- `app/fae/src/app.rs`
