# FAE Agent Recipes

## 1. 创建磁盘配置

```rust
use fae_agent::{SingleAgentConfig, SingleAgentInfo, SingleAgentModelConfig};
use std::collections::HashMap;

async fn save_agent(home: &std::path::Path) -> anyhow::Result<()> {
    let agent_id = "reviewer";
    let directory = home.join("agents");
    tokio::fs::create_dir_all(&directory).await?;

    let config = SingleAgentConfig {
        agent: SingleAgentInfo {
            name: agent_id.to_string(),
            user_id: "local".to_string(),
            session_id: "review-session".to_string(),
            metadata: HashMap::new(),
        },
        model: SingleAgentModelConfig {
            model: "gpt-xxx".to_string(),
            context_size: 32_000,
            history_turns: 20,
            max_completion_tokens: Some(4_096),
            temperature: Some(0.2),
            max_tool_iterations: 8,
        },
        tools: vec!["read_file".to_string()],
        skills: Vec::new(),
        mcp_servers: Vec::new(),
    };

    tokio::fs::write(
        directory.join(format!("{agent_id}_config.json")),
        serde_json::to_vec_pretty(&config)?,
    )
    .await?;
    tokio::fs::write(
        directory.join(format!("{agent_id}_prompt.txt")),
        "Review code for correctness and provide concise findings.",
    )
    .await?;
    Ok(())
}
```

不要手工拼接未经校验的外部输入作为 agent ID。库会拒绝目录组件，但创建配置的一侧也应使用可信
ID。

## 2. 单轮调用

只等待执行结果：

```rust
let engine = fae_engine::Engine::default().await;
let (env, _) = SingleAgentEnv::from_agent_id("reviewer", "Review src/lib.rs");
let (_, ()) = engine.invoke(env).await?;
engine.exit().await?;
```

`invoke` 适合不需要展示增量输出的后台任务。需要 UI、日志或工具状态时使用 `launch`。

## 3. 流式消费

```rust
async fn consume_turn(session: &SingleAgentSession) -> anyhow::Result<String> {
    let mut completed = None;
    while let Some(event) = session.answer().await? {
        match event.data {
            SessionEventData::ModelOutput { content } => {
                print!("{content}");
            }
            SessionEventData::ToolCall { arguments, .. } => {
                eprintln!("tool {}: {arguments}", event.source);
            }
            SessionEventData::ToolOutput {
                output,
                completed,
                ..
            } => {
                eprintln!("tool {} [{completed}]: {output}", event.source);
            }
            SessionEventData::Completed { content } => {
                completed = Some(content);
                break;
            }
            SessionEventData::Failed { error } => anyhow::bail!(error),
            _ => {}
        }
    }
    completed.ok_or_else(|| anyhow::anyhow!("agent session ended without a result"))
}

let (env, session) =
    SingleAgentEnv::from_agent_id("reviewer", "Review src/lib.rs");
let execution = engine.launch(env).await?;
let answer = consume_turn(&session).await?;
execution.result::<()>().await?;
```

不要只等待 `execution.result()` 后再消费事件；长输出可能无法实时展示，也会失去及时处理失败和
工具状态的机会。

## 4. 多轮对话

```rust
let (env, session) =
    SingleAgentEnv::from_agent_id("reviewer", "Find the highest-risk issue.");
let first = engine.launch(env).await?;
consume_turn(&session).await?;
first.result::<()>().await?;

session
    .call("Show a minimal patch for that issue.".to_string())
    .await?;
consume_turn(&session).await?;
```

多轮必须复用首次 `SingleAgentEnv` 返回的 session。重新创建 ENV 会重新加载配置并创建新的运行时
session；即使持久化 ID 相同，也不是同一个活跃会话句柄。

## 5. 自定义 FAE Home

测试和嵌入应用中应让所有组件共享一个 home：

```rust
let home = temp_dir.path();
let mut builder = EngineBuilder::new();

builder.add_runtime(PlanRuntime::new());
builder.add_runtime(ModelRuntime::new());
builder.add_runtime(SessionRuntime::with_host_dir(home));
builder.add_runtime(SkillRuntime::with_host_dir(home));
builder.add_runtime(McpRuntime::with_mcp_dir(home.join("mcp")));

let mut tools = ToolsRuntime::new();
tools.add_tool(Box::new(DefaultTools::default()));
builder.add_runtime(tools);

builder.add_plan_builder(SingleAgentPlanBuilder::with_home_dir(home));
let engine = builder.build().await;
```

不要只给 `SingleAgentPlanBuilder` 设置测试目录而让 `SessionRuntime`、`SkillRuntime` 或
`McpRuntime` 继续使用默认 home。

## 6. 配置 Tool

JSON：

```json
{
  "tools": ["read_file", "execute_command"]
}
```

引擎：

```rust
let mut tools = ToolsRuntime::new();
tools.add_tool(Box::new(DefaultTools::default()));
builder.add_runtime(tools);
```

排查顺序：

1. 配置名称是否与 runtime 注册名称完全一致。
2. runtime 的查询结果能否反序列化为 `FunctionObject`。
3. 模型返回的函数名是否存在于 builder 建立的 route 中。
4. streaming 工具是否最终发送 completed output。
5. `max_tool_iterations` 是否足够且有合理上限。

## 7. 配置 Skill

按名称加载：

```json
{
  "skills": [
    { "type": "name", "value": "weather" }
  ]
}
```

对应文件：

```text
${FAE_HOST:-~/.fae}/skills/weather/SKILL.md
```

按路径加载：

```json
{
  "skills": [
    {
      "type": "path",
      "value": "/workspace/docs/skills/weather/SKILL.md"
    }
  ]
}
```

必须注册 `SkillRuntime`。Skill 只向 prompt 注入 metadata 和读取路径；若 Skill 依赖某个工具，
仍需在 agent config 的 `tools` 或 `mcp_servers` 中配置并注册对应 runtime。

## 8. 配置 MCP

Agent JSON：

```json
{
  "mcp_servers": ["maps"]
}
```

MCP 配置文件位于 FAE home 的 `mcp` 目录，内容使用 `mcpServers`：

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

远程 server：

```json
{
  "mcpServers": {
    "maps": {
      "url": "https://example.test/mcp",
      "headers": {
        "Authorization": "Bearer token"
      }
    }
  }
}
```

不要把真实 token 提交到仓库。使用部署环境生成配置或其他秘密注入机制。

## 9. 预检配置

在启动 UI 或长任务前先加载：

```rust
let source = SingleAgentSource::AgentId("reviewer".into());
let loader = SingleAgentPlanBuilder::new();
let (config, prompt) = loader.load_config(&source).await?;

println!("model={}", config.model.model);
anyhow::ensure!(!prompt.trim().is_empty(), "prompt must not be empty");
```

该步骤适合尽早报告文件缺失、JSON 错误和字段校验错误。它不会验证外部模型是否可连接，也不会
执行 Tool、Skill 或 MCP 查询。

## 10. Workflow Agent 节点

```rust
builder.execute(
    "review",
    WorkflowAction::SingleAgent {
        source: SingleAgentSource::AgentId("reviewer".into()),
        input: json!("Review: {$input.content}"),
    },
    "end",
)?;
```

Agent 的最终文本是该节点输出。流式事件会转发到 `WorkflowSession`，并附带 workflow ID、
node ID 和 turn ID。

## 11. 测试模式

配置加载测试不应访问用户真实的 `~/.fae`：

```rust
#[tokio::test]
async fn loads_agent_config() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let agents = home.path().join("agents");
    tokio::fs::create_dir_all(&agents).await?;
    tokio::fs::write(
        agents.join("reviewer_config.json"),
        serde_json::to_vec(&fixture_config())?,
    )
    .await?;
    tokio::fs::write(
        agents.join("reviewer_prompt.txt"),
        "Review carefully.",
    )
    .await?;

    let builder = SingleAgentPlanBuilder::with_home_dir(home.path());
    let (config, prompt) = builder
        .load_config(&SingleAgentSource::AgentId("reviewer".into()))
        .await?;

    assert_eq!(config.agent.name, "reviewer");
    assert_eq!(prompt, "Review carefully.");
    Ok(())
}
```

业务逻辑测试应使用 fake model/tool runtime，避免真实网络调用。至少覆盖：

- ID 与显式路径加载。
- 非法 ID 和错误 JSON。
- 空 model、零 `context_size`、零 `max_tool_iterations`。
- Tool/MCP 名称冲突。
- 无工具回复、工具调用回复和达到迭代上限。
- session 首轮、后续轮次和失败事件。

## 12. 排错清单

### 找不到配置或 prompt

- `FAE_HOST` 是否指向预期目录。
- 文件是否位于 `<home>/agents`。
- 文件名是否严格为 `<agent-id>_config.json` 和 `<agent-id>_prompt.txt`。
- 使用显式路径时，调用方是否正确展开 `~`。

### 配置解析或校验失败

- JSON 是否符合 `SingleAgentConfig` 的嵌套结构。
- `skills` 是否使用 `{"type","value"}`，而不是纯字符串。
- `agent.name` 是否与 ID 一致。
- `model`、`user_id`、`session_id` 是否非空。
- `context_size` 和 `max_tool_iterations` 是否大于 0。

### runtime 不支持任务

- 是否注册 `PlanRuntime`、`ModelRuntime`、`SessionRuntime` 和 builder。
- 配置声明 Tool、Skill、MCP 后，是否注册相应 runtime。
- 自定义 runtime 的 `TaskType` 是否匹配请求。
- 所有磁盘 runtime 是否使用同一个 home。

### 模型未调用工具

- 工具是否出现在 agent JSON 的 `tools` 或 `mcp_servers`。
- system prompt 是否明确说明何时使用工具。
- 工具 schema 是否有效，名称是否冲突。
- 模型本身是否支持 function calling。

### 会话历史不符合预期

- `agent.user_id` 与 `agent.session_id` 是否稳定。
- `history_turns` 是否为 0 或过小。
- `SessionRuntime` 是否与 agent builder 使用相同 home。
- 是否误创建新 ENV，而不是复用已绑定的 session。

### 事件停止或执行不结束

- 是否持续调用 `session.answer()`。
- 是否仅在 `event.is_terminal()` 后退出消费循环。
- Tool/MCP runtime 的异步路径是否总会发布成功或失败事件。
- 是否在工具调用期间不断追加输入，导致当前轮继续处理 pending queue。
