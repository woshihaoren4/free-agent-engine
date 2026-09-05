# FAE Workflow API

## 1. 核心类型

| 类型 | 职责 |
| --- | --- |
| `WorkflowMetadata` | 可序列化的 Workflow 定义，当前 `version` 为 `1` |
| `WorkflowMetadataBuilder` | 构造并校验 Workflow 图 |
| `WorkflowMetadataLoader` | 按 `workflow_id` 异步加载 metadata |
| `FAEWorkflowMetadataLoader` | 内存注册表 + `{FAE_HOST}/workflows` 文件加载 |
| `WorkflowPlanBuilder` | 将 `WorkflowEnv` 和 metadata 转成可执行 `Plan` |
| `WorkflowRuntime` | 接收 `TaskType::Workflow` 并驱动 workflow plan |
| `WorkflowEnv` | 一次执行的 `workflow_id`、输入和 session |
| `WorkflowSession` | 输出节点事件、Agent 流式事件和最终结果 |

## 2. Builder API

```rust
let mut builder = WorkflowMetadataBuilder::new("workflow-id");

builder.start("start", "next")?;
builder.start("start", ["left", "right"])?; // fan-out
builder.start_parallel("start", ["left", "right"])?; // start 的便捷别名

builder.execute("node", action, "next")?;
builder.execute("node", action, ["left", "right"])?; // action 完成后 fan-out

builder.decision("route", condition, "yes", "no")?;
builder.decision("route", condition, ["a", "b"], ["c"])?;

builder.loop_node("retry", condition, "body", "end", 5)?;

builder.end("end", Some(json!({ "result": "{$node}" })))?;
builder.join_end("end", None)?; // end 的便捷别名

let metadata = builder.build()?;
```

`start_parallel` / `join_end` 最终生成与显式 `ParallelStart` / `JoinEnd` 语义兼容的图。
当前 builder 的便捷方法实际使用普通 `Start` / `End` 变体；不要依赖变体名称判断是否并行，
应依据多目标边和多前驱结构。

## 3. 节点

### Start

发出输入作为节点输出，再激活 `next`。一个目标进入顺序执行；多个目标进入 DAG 执行。

### Execute

解析 action 中的模板，提交一个 runtime task，保存返回值为该节点输出，再激活 `next`。

### Decision

计算条件并选择 `on_true` 或 `on_false`。条件节点输出为布尔值。

### Loop

条件为真时递增 iteration 并进入 `body`，条件为假时进入 `next`。循环体必须有路径回到此节点。
达到 `max_iterations` 后条件仍为真会报错。

### End

结束流程。存在 `output` 时先解析模板；省略时：

- 顺序流程返回最后一个 action 输出，没有 action 时返回输入。
- DAG 返回直接活跃前驱的输出；多个输出以节点 ID 为 key 组成对象。生产代码建议显式设置
  `output`，避免图调整改变隐式结果。

## 4. Conditions

```rust
WorkflowCondition::Truthy {
    value: json!("{$input.enabled}"),
}

WorkflowCondition::Exists {
    value: json!("{$optional_node.value}"),
}

WorkflowCondition::Compare {
    left: json!("{$input.count}"),
    op: WorkflowCompare::Ge,
    right: json!(3),
}
```

`Truthy` 的假值是 `null`、`false`、数字零、空字符串、空数组和空对象。

`Compare` 支持 `Eq`、`Ne`、`Gt`、`Ge`、`Lt`、`Le`。有序比较只接受两个数字或两个字符串；
`Eq` / `Ne` 使用 JSON 值相等。

`Exists` 只判断引用能否解析。它适合检查可能未执行的分支输出，但不要在后续模板中无条件读取
该输出。

## 5. Actions 与依赖

### Tool

```rust
WorkflowAction::Tool {
    tool_name: READ_FILE.to_string(),
    arguments: json!({
        "path": "{$input.path}",
        "max_bytes": 4096
    }),
}
```

要求引擎注册能路由该工具名的 `ToolsRuntime`。工具 completed output 若是有效 JSON，会解析为
JSON 值；否则保存为字符串。streaming items 会被消费，但节点只保存 completed output。

### Workflow

```rust
WorkflowAction::Workflow {
    workflow_id: "child-workflow".to_string(),
    input: json!({ "value": "{$input.child_value}" }),
}
```

父子流程必须能由同一 loader 加载。子流程与父流程共享 root context，父节点输出是子流程最终
JSON 值。

### SingleAgent

```rust
WorkflowAction::SingleAgent {
    source: SingleAgentSource::AgentId("reviewer".into()),
    input: json!("Review: {$input.content}"),
}
```

该配置从 `${FAE_HOST:-~/.fae}/agents/reviewer_config.json` 加载，system prompt
从同目录的 `reviewer_prompt.txt` 加载。也可使用 `SingleAgentSource::Paths`
显式指定两个文件。

要求 `PlanRuntime`、`SingleAgentPlanBuilder`、`ModelRuntime`，并按实际能力注册
`SessionRuntime`、`ToolsRuntime`、`SkillRuntime` 和 `McpRuntime`。Agent 最终文本保存为节点
字符串输出；模型推理、输出和工具事件会转发到 workflow session，并带 `workflow_id` 与
`node_id`。

### Session

```rust
WorkflowAction::Session {
    request: SessionRequest::Query {
        user: "alice".into(),
        session_id: "session-1".into(),
        limit: Some(20),
        offset: None,
    },
}
```

要求 `SessionRuntime`。请求会先经过模板解析，响应序列化为节点 JSON 输出。

### Python

```rust
WorkflowAction::Python {
    code: "result = arguments['value'] * 2".into(),
    arguments: json!({ "value": "{$input.value}" }),
    task_type: "workflow.python".into(),
}
```

FAE 不会直接执行 Python。此 action 被包装为：

```rust
WorkflowActionRequest {
    action: "python".to_string(),
    payload: json!({ "code": code, "arguments": resolved_arguments }),
}
```

必须注册支持 `TaskType::Any(task_type)` 的 runtime，并返回：

```rust
WorkflowActionResponse { output: result }
```

### Custom

```rust
WorkflowAction::Custom {
    task_type: "my.workflow.action".into(),
    request: json!({ "id": "{$input.id}" }),
}
```

与 Python 使用相同扩展协议，但 `action` 为 `"custom"`，`payload` 是解析后的 `request`。
`task_type` 不得为空。

## 6. 值模板

解析器递归处理 JSON 数组和对象的 value，不处理对象 key。

| 表达式 | 含义 |
| --- | --- |
| `{$input}` | 完整输入 |
| `{$input.user.name}` | 输入对象字段 |
| `{$input.items.0}` | 数组元素 |
| `{$read.content}` | `read` 节点输出字段 |
| `{$last.remaining}` | 顺序流程的最近 action 输出 |
| `{$loop.retry.iteration}` | `retry` 循环当前次数 |

精确引用保留原 JSON 类型，嵌入字符串会用字符串值或 JSON 表示进行插值。字段缺失、数组越界、
从标量继续取字段、读取尚未完成的节点都会报错。

并行分支只能读取确定已经完成的祖先节点。汇合后的节点可以读取所有被选择分支的输出。

## 7. Loader 与持久化

```rust
let loader = FAEWorkflowMetadataLoader::new();
loader.add(metadata.clone())?;
loader.remove("workflow-id")?;

metadata.save_json("/tmp/workflow.json").await?;
let loaded = WorkflowMetadata::load_json("/tmp/workflow.json").await?;
let text = metadata.to_json()?;
let parsed = WorkflowMetadata::from_json(&text)?;
```

默认根目录：

```text
${FAE_HOST:-~/.fae}/workflows/<workflow-id>.json
```

`FAE_HOST` 支持 `~` 和 `~/...` 展开。内存注册优先于磁盘。磁盘文件的 `id` 必须与请求的
`workflow_id` 相同；ID 必须是单个非空路径组件，不能包含目录。

## 8. 引擎配置

最小 workflow 引擎：

```rust
let loader = FAEWorkflowMetadataLoader::new();
let mut builder = EngineBuilder::new();
builder.add_runtime(PlanRuntime::new());
builder.add_runtime(WorkflowRuntime::with_metadata_loader(loader.clone()));
builder.add_plan_builder(WorkflowPlanBuilder::new(loader.clone()));
let engine = builder.build().await;
```

按 action 增加 runtime 和 plan builder。注意 `Engine::default().await` 当前注册
`WorkflowRuntime`，但不注册 `WorkflowPlanBuilder`；不要假定默认引擎可直接执行 workflow。

metadata 查询：

```rust
let metadata = engine
    .rt()
    .select::<String, WorkflowMetadata>(
        TaskType::Workflow,
        "workflow-id".to_string(),
    )
    .await?;
```

## 9. 执行与事件

等待结果：

```rust
let (env, session) = WorkflowEnv::new("workflow-id", input);
let (_, output) = engine.invoke::<_, Value>(env).await?;
```

实时消费事件：

```rust
let (env, session) = WorkflowEnv::new("workflow-id", input);
let execution = engine.launch(env).await?;

while let Some(event) = session.answer().await? {
    let terminal = event.is_terminal();
    println!("{} {:?}", event.kind(), event.data);
    if terminal {
        break;
    }
}

let output = execution.result::<Value>().await?;
```

也可以通过 `session.result().await?` 获取最终 JSON；该调用不会消费终止事件。

Workflow 控制节点与 action 完成时发送 `NodeCompleted`。终止 end 事件的 `finished` 为 `true`。
失败发送 `Failed`。SingleAgent 的流式事件也进入同一 session，但带 `turn_id`，不会被误判为
整个 workflow 的终止事件。

## 10. JSON 结构

最小磁盘定义：

```json
{
  "version": 1,
  "id": "echo-input",
  "nodes": {
    "start": {
      "type": "start",
      "next": "end"
    },
    "end": {
      "type": "end",
      "output": {
        "received": "{$input}"
      }
    }
  }
}
```

`Start`、`Execute`、`Decision` 的目标字段兼容单个字符串和字符串数组。序列化后的新定义通常使用
数组。手写 JSON 后必须调用 `WorkflowMetadata::from_json` 或 `validate()`，不要绕过校验。
