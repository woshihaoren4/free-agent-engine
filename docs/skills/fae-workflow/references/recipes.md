# FAE Workflow Recipes

## 1. 串行 Tool Workflow

```rust
fn build_read_workflow() -> anyhow::Result<WorkflowMetadata> {
    let mut builder = WorkflowMetadataBuilder::new("read-workflow");
    builder.start("start", "read")?;
    builder.execute(
        "read",
        WorkflowAction::Tool {
            tool_name: READ_FILE.to_string(),
            arguments: json!({
                "path": "{$input.path}",
                "max_bytes": 8192
            }),
        },
        "end",
    )?;
    builder.end(
        "end",
        Some(json!({
            "path": "{$read.path}",
            "content": "{$read.content}",
            "truncated": "{$read.truncated}"
        })),
    )?;
    builder.build()
}
```

适合需要确定顺序和前一步输出的流程。优先显式定义 end output。

## 2. 条件分支

```rust
let mut builder = WorkflowMetadataBuilder::new("approval");
builder.start("start", "route")?;
builder.decision(
    "route",
    WorkflowCondition::Compare {
        left: json!("{$input.score}"),
        op: WorkflowCompare::Ge,
        right: json!(80),
    },
    "approved",
    "rejected",
)?;
builder.execute("approved", approved_action, "end")?;
builder.execute("rejected", rejected_action, "end")?;
builder.end(
    "end",
    Some(json!({
        "approved": "{$route}",
        "input": "{$input}"
    })),
)?;
```

不要在 end 中无条件引用只在一个分支执行的节点。可直接读取 decision 节点的布尔输出，或让两个
分支输出统一结构并在汇合节点中处理。

## 3. 并行 Fan-out 与 Join

```rust
let mut builder = WorkflowMetadataBuilder::new("parallel-review");
builder.start("start", ["review_code", "review_manifest"])?;
builder.execute("review_code", code_action, "summarize")?;
builder.execute("review_manifest", manifest_action, "summarize")?;
builder.execute(
    "summarize",
    WorkflowAction::Custom {
        task_type: "review.summary".into(),
        request: json!({
            "code": "{$review_code}",
            "manifest": "{$review_manifest}"
        }),
    },
    "end",
)?;
builder.end("end", Some(json!("{$summarize}")))?;
```

`summarize` 有两个前驱，因此只有两个分支都完成后才执行。分支返回顺序不保证与提交顺序一致，
不要依赖完成顺序。

可在任意 `Execute` 后 fan-out：

```rust
builder.execute("load", load_action, ["left", "right"])?;
```

条件也可以一次选择多个目标：

```rust
builder.decision("route", condition, ["check_a", "check_b"], ["skip"])?;
```

## 4. 有界循环

```rust
let mut builder = WorkflowMetadataBuilder::new("bounded-retry");
builder.start("start", "initialize")?;
builder.execute(
    "initialize",
    state_action(json!({ "remaining": "{$input.rounds}" })),
    "retry",
)?;
builder.loop_node(
    "retry",
    WorkflowCondition::Compare {
        left: json!("{$last.remaining}"),
        op: WorkflowCompare::Gt,
        right: json!(0),
    },
    "decrement",
    "end",
    10,
)?;
builder.execute(
    "decrement",
    state_action(json!({
        "remaining": "{$last.remaining}",
        "iteration": "{$loop.retry.iteration}"
    })),
    "retry",
)?;
builder.end(
    "end",
    Some(json!({
        "iterations": "{$loop.retry.iteration}",
        "state": "{$last}"
    })),
)?;
```

循环注意事项：

- 初始化 action 必须先生成条件需要的状态。
- 循环体必须返回 loop 节点。
- `max_iterations` 是故障保护，不是期望次数；应根据输入设置合理上限。
- Loop 不能与 fan-out 或多前驱 join 出现在同一个 workflow。需要二者时拆成父子 workflow。
- 当循环可能零次执行时，`{$loop.retry.iteration}` 尚不存在。若 end 必须支持零次，避免直接引用
  iteration，或在进入 loop 前生成可用的默认状态。

## 5. 父子 Workflow

```rust
fn child() -> anyhow::Result<WorkflowMetadata> {
    let mut child = WorkflowMetadataBuilder::new("validate-order");
    child.start("start", "end")?;
    child.end(
        "end",
        Some(json!({
            "order_id": "{$input.order_id}",
            "status": "validated"
        })),
    )?;
    child.build()
}

fn parent() -> anyhow::Result<WorkflowMetadata> {
    let mut parent = WorkflowMetadataBuilder::new("process-order");
    parent.start("start", "validate")?;
    parent.execute(
        "validate",
        WorkflowAction::Workflow {
            workflow_id: "validate-order".into(),
            input: json!({ "order_id": "{$input.order.id}" }),
        },
        "end",
    )?;
    parent.end("end", Some(json!("{$validate}")))?;
    parent.build()
}

loader.add(parent()?)?;
loader.add(child()?)?;
```

父子流程共享同一 loader。为了组合并行和循环，可让 DAG 父流程调用包含 Loop 的顺序子流程。

## 6. 从磁盘加载

生成并保存：

```rust
let metadata = build_workflow()?;
let root = std::env::var_os("FAE_HOST")
    .map(PathBuf::from)
    .unwrap_or_else(|| dirs::home_dir().unwrap().join(".fae"));
let directory = root.join("workflows");
tokio::fs::create_dir_all(&directory).await?;
metadata
    .save_json(directory.join(format!("{}.json", metadata.id)))
    .await?;
```

运行时不需要预先 `add`：

```rust
let loader = FAEWorkflowMetadataLoader::new();
let (env, _) = WorkflowEnv::new("workflow-id", input);
```

测试中使用隔离目录：

```rust
let loader = FAEWorkflowMetadataLoader::with_home_dir(temp_dir);
```

## 7. 自定义 Action Runtime

Python 与 Custom action 都通过 `WorkflowActionRequest` / `WorkflowActionResponse` 扩展协议执行：

```rust
#[derive(Debug)]
struct ActionRuntime {
    event_sender: Sender<Event>,
    event_receiver: Receiver<Event>,
}

#[async_trait::async_trait]
impl RuntimeSelectExec<WorkflowActionRequest, WorkflowActionResponse, (), ()>
    for ActionRuntime
{
    fn id(&self) -> &str {
        "workflow.action"
    }

    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Any("workflow.action".to_string())]
    }

    async fn watch(&self) -> fae_agent::Result<Receiver<Event>> {
        Ok(self.event_receiver.clone())
    }

    async fn exec(
        &self,
        task: TaskReq<WorkflowActionRequest>,
    ) -> fae_agent::Result<TaskResp<WorkflowActionResponse>> {
        let output = execute_action(&task.req.action, &task.req.payload).await?;
        Ok(TaskResp {
            ctx: task.ctx,
            meta: task.meta,
            resp: WorkflowActionResponse { output },
        })
    }

    async fn spawn(
        &self,
        task: TaskReq<WorkflowActionRequest>,
    ) -> fae_agent::Result<()> {
        // 按 RuntimeSelectExec 契约异步执行，并向 event_sender 发送
        // EventType::TaskResult 或 EventType::TaskError。
        todo!()
    }
}
```

action 的 `task_type` 必须与 runtime 的 `TaskType::Any(...)` 完全一致。完整 spawn 实现参考
`examples/workflow.rs` 的 `PythonActionRuntime`。

## 8. 实时事件与结果

当 workflow 包含 SingleAgent 或耗时 action 时：

```rust
let (env, session) = WorkflowEnv::new("workflow-id", input);
let execution = engine.launch(env).await?;

let events = async {
    while let Some(event) = session.answer().await? {
        match &event.data {
            SessionEventData::ModelOutput { content } => print!("{content}"),
            SessionEventData::NodeCompleted { output, .. } => {
                println!("{}: {output}", event.node_id.as_deref().unwrap_or("-"));
            }
            SessionEventData::Failed { error } => eprintln!("{error}"),
            _ => {}
        }
        if event.is_terminal() {
            break;
        }
    }
    anyhow::Ok(())
};

let (output, ()) = tokio::try_join!(execution.result::<Value>(), events)?;
```

不要先调用 `invoke().await` 再开始消费实时事件；那只能在执行完成后读取积压事件。测试可以用
`tokio::try_join!` 并发执行 `invoke` 与事件消费。

## 9. 测试模式

metadata 单元测试：

```rust
#[test]
fn workflow_is_valid() -> anyhow::Result<()> {
    let workflow = build_workflow()?;
    assert_eq!(workflow.id, "workflow-id");
    assert!(workflow.nodes.contains_key("end"));

    let json = workflow.to_json()?;
    WorkflowMetadata::from_json(&json)?;
    Ok(())
}
```

集成执行测试：

```rust
#[tokio::test]
async fn workflow_returns_expected_output() -> anyhow::Result<()> {
    let loader = FAEWorkflowMetadataLoader::new();
    loader.add(build_workflow()?)?;
    let engine = build_test_engine(loader).await;

    let (env, session) = WorkflowEnv::new("workflow-id", json!({ "value": 42 }));
    let (_, output) = engine.invoke::<_, Value>(env).await?;
    assert_eq!(output, json!({ "value": 42 }));

    let mut terminal_seen = false;
    while let Some(event) = session.answer().await? {
        if event.is_terminal() {
            terminal_seen = true;
            break;
        }
    }
    assert!(terminal_seen);
    engine.exit().await?;
    Ok(())
}
```

避免在 metadata 测试中调用真实模型、网络服务或外部 Python。为 Custom action 注册确定性的
fixture runtime。

## 10. 排错清单

### `workflow` 无法构建

- 检查 start/end 是否各一个。
- 检查所有 target 拼写。
- 检查是否存在不可达节点或无法到 end 的节点。
- 检查普通环是否错误绕过 `loop_node`。
- 检查 fan-out/DAG 是否混入 Loop。

### runtime 不支持任务

- `WorkflowRuntime`、`WorkflowPlanBuilder` 是否都已注册。
- 两者是否共享同一个 loader。
- action 需要的 runtime 是否已注册。
- Custom/Python 的 `task_type` 是否与 `TaskType::Any` 一致。
- Tool name、Skill query 或 MCP server name 是否可被对应 runtime 查询。

### 模板解析失败

- 被引用节点是否在当前节点之前完成。
- 条件未选中的分支是否被引用。
- 对象字段和数组索引是否存在。
- JSON 数字是否被误写成嵌入式字符串。
- DAG 中是否错误使用 `{$last}`。

### 并行流程卡住

- 每个活跃分支是否都能到达汇合点。
- action runtime 的 `spawn` 是否总会发送成功或失败事件。
- task response 是否保留原始 `TaskMeta`，尤其是 `meta.id`。
- 是否有 action runtime 只实现 `exec`，却没有正确实现异步 `spawn`。

### 事件看不到或结果一直等待

- 是否持有由同一次 `WorkflowEnv::new` 返回的 session。
- 是否在执行期间并发调用 `session.answer()`。
- runtime 错误路径是否发送 `TaskError`。
- 消费循环是否仅在 `event.is_terminal()` 时退出。
