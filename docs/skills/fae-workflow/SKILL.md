---
name: "fae-workflow"
description: "Builds, runs, and troubleshoots FAE workflows. Invoke when adding workflow graphs, actions, conditions, persistence, execution, or event handling."
---

# FAE Workflow

使用本 Skill 在当前仓库中设计、实现、运行或排查 FAE Workflow。

## 适用范围

在以下任务中调用本 Skill：

- 新建或修改 `WorkflowMetadata` / Workflow JSON。
- 编排串行、条件、并行、汇合、循环或父子流程。
- 配置 `WorkflowRuntime`、`WorkflowPlanBuilder` 和 `FAEWorkflowMetadataLoader`。
- 使用 Tool、SingleAgent、Session、Python、Custom action。
- 消费 `WorkflowSession` 事件，或排查模板解析、图校验和运行时路由问题。

不要把普通 Rust `Plan` 实现当作 Workflow。用户明确需要手写 `Plan` 状态机时，直接使用
`Plan` / `PlanBuilderWithEnv` API。

## 执行步骤

1. 阅读目标代码和相邻示例，确认工作树中的 API 名称；Workflow 仍在演进，不要凭旧接口实现。
2. 选择最简单的图结构。只有确实需要并发时才使用多目标边，只有需要有界重试时才使用
   `loop_node`。
3. 使用 `WorkflowMetadataBuilder` 构图，并通过 `build()` 执行完整校验。
4. 为每种 action 注册对应 runtime。`WorkflowRuntime` 和 `WorkflowPlanBuilder` 必须共享同一个
   metadata loader。
5. 注册 metadata，或保存到 `{FAE_HOST}/workflows/<workflow-id>.json`。
6. 使用 `WorkflowEnv::new` 启动流程；需要实时事件时优先使用 `engine.launch` 并并发消费
   `WorkflowSession`。
7. 添加与改动风险匹配的测试，并运行 `cargo fmt --check`、目标测试和 `cargo check`。

## 开始前按需阅读

- 节点、动作、模板语法、加载和运行时契约：
  [references/workflow-api.md](references/workflow-api.md)
- 常见完整配方和排错清单：
  [references/recipes.md](references/recipes.md)

若实现涉及 SingleAgent、Python/Custom action、并行图或磁盘 JSON，必须先阅读对应参考章节。

## 最小可运行模式

```rust
use fae_agent::{
    FAEWorkflowMetadataLoader, WorkflowAction, WorkflowEnv, WorkflowMetadataBuilder,
};
use fae_engine::{EngineBuilder, PlanRuntime, ToolsRuntime, WorkflowRuntime, READ_FILE};
use serde_json::{Value, json};

async fn run(path: &str) -> anyhow::Result<Value> {
    let mut workflow = WorkflowMetadataBuilder::new("read-file");
    workflow.start("start", "read")?;
    workflow.execute(
        "read",
        WorkflowAction::Tool {
            tool_name: READ_FILE.to_string(),
            arguments: json!({ "path": "{$input.path}", "max_bytes": 4096 }),
        },
        "end",
    )?;
    workflow.end("end", Some(json!("{$read.content}")))?;

    let loader = FAEWorkflowMetadataLoader::new();
    loader.add(workflow.build()?)?;

    let mut engine = EngineBuilder::new();
    engine.add_runtime(PlanRuntime::new());
    engine.add_runtime(WorkflowRuntime::with_metadata_loader(loader.clone()));
    let mut tools = ToolsRuntime::new();
    tools.add_tool(Box::new(fae_engine::DefaultTools::default()));
    engine.add_runtime(tools);
    engine.add_plan_builder(fae_agent::WorkflowPlanBuilder::new(loader));
    let engine = engine.build().await;

    let (env, _) = WorkflowEnv::new("read-file", json!({ "path": path }));
    let (_, output) = engine.invoke::<_, Value>(env).await?;
    engine.exit().await?;
    Ok(output)
}
```

关键点：

- 不能只注册 `WorkflowRuntime`；执行还需要 `PlanRuntime` 和 `WorkflowPlanBuilder`。
- `Tool` action 需要能处理对应工具名的 `ToolsRuntime`。
- `end(..., None)` 返回最近 action 的输出；若没有 action 输出，则返回 workflow 输入。

## 图设计约束

- 必须恰好有一个 start 和一个 end。
- 每个节点必须可从 start 到达，并且必须存在到 end 的路径。
- 所有目标节点必须存在；任何边都不能返回 start。
- 普通环非法。环必须通过 `Loop` 节点形成，且循环体必须返回该 Loop。
- `max_iterations` 必须大于零。
- fan-out 或多前驱汇合会启用 DAG 执行器；DAG 不能包含 Loop。
- 汇合节点会等待所有前驱边完成判定；未选中的条件分支会被标记为 inactive，不会阻塞汇合。

## 模板规则

在任意 action 参数、条件或 end 输出中使用：

- `{$input.field}`：workflow 输入。
- `{$node_id.field}`：已完成节点的输出。
- `{$last.field}`：顺序执行器中最近 action 的输出。
- `{$loop.loop_id.iteration}`：当前循环次数，从 1 开始。

完整字符串只有一个引用时保留 JSON 类型：

```rust
json!({ "count": "{$input.count}" }) // 结果仍是数字
```

引用嵌入普通文本时结果为字符串：

```rust
json!("count={$input.count}") // 结果是 "count=3"
```

不要在并行 DAG 中使用 `{$last...}`；DAG 执行器没有全局“最近输出”。

## 验证

至少运行：

```bash
cargo fmt --check
cargo test -p fae-agent workflow
cargo check --workspace
```

若修改示例，再运行对应目标：

```bash
cargo test -p examples --example parent_child_workflow
cargo test -p examples --example workflow
```

需要模型的完整示例通过以下方式运行：

```bash
cargo run -p examples --example workflow
```

## 当前源码入口

- `crates/fae-agent/src/workflow/definition.rs`
- `crates/fae-agent/src/workflow/builder.rs`
- `crates/fae-agent/src/workflow/value.rs`
- `crates/fae-agent/src/workflow/plan.rs`
- `crates/fae-agent/src/workflow/session.rs`
- `crates/fae-engine/src/engine_rt/workflow_runtime.rs`
- `examples/workflow_metadata.rs`
- `examples/workflow.rs`
- `examples/parent_child_workflow.rs`
