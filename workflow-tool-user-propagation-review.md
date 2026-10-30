# Context 重构与身份传播验收审查

> 后续修复验证（2026-10-30）：下述两项问题均已修复。Plan 回调不再提前提交流终态；五类异步 runtime 补齐实际 Started 记录。新增 3 项集成回归修复前失败、修复后通过，全部 154 项本地测试及工作区格式/编译检查通过。以下保留修复前的审查证据和结论。

审查日期：2026-10-30。基线：`3629748d91ed978723f001c35051f22886b42b42`。
范围：`docs/faeplan/workflow-tool-user-propagation.md`、当前 36 个已跟踪变更文件、未跟踪的 `context_integration.rs` 及直接相关运行时。

结论：主要设计已落实，但尚未完全满足文档验收要求。确认 1 项 P1 和 1 项 P2；未发现 P0。第 12 节的编译与 151 项测试结果可复现，但测试通过不足以证明全部验收场景成立。建议解决以下问题后再将状态认定为完整验收通过。

## 已符合的主要要求

- Context trait 以 `state` / `set_state` 管理生命周期；成功输出单次消费，类型不符保留输出，消费后保留 Succeeded。
- 根作用域一次绑定；clone 共享状态，fork 继承用户并拥有独立状态。父取消与 fork 注册由同一局部锁协调。
- Agent/Workflow/Memory/Todo 的执行身份来自 Context；身份专用 invocation 已删除，Agent 专用目标/source/祖先链保留。
- 顺序与 DAG Workflow 的工具委派、alice/bob 分区隔离、子 Agent session override 和下一回合的新 Context 有集成覆盖。
- Session/Memory 执行请求校验 Context 分区，Todo 缺 Agent 归属时拒绝。
- Workflow End 的 After hook 在成功状态与事件前执行；Plan 清理保留 abort 回调。
- 栈记录结构化，支持全局序号、Meta、作用域、分页、过滤、快照和容量错误。

## 1. P1：Plan 回调提前完成仍未消费的流

置信度：10/10。位置：`crates/fae-engine/src/engine_rt/plan_runtime.rs:535-546`。
关联要求：第 6.2、8.5 节，K3/S5。

```rust
if matches!(task_meta.ty, TaskType::Tool | TaskType::Model) {
    let state = match &next {
        Ok(_) => ContextState::succeeded(()),
        Err(error) => ContextState::failed(error.to_string()),
    };
    if task_ctx.set_state(state).is_ok() {
        // append terminal phase
    }
}
```

`plan.next` 返回成功只证明计划回调已处理。自定义 Plan 可以保留响应，或交由其他协程消费，然后继续派发任务。这里提前提交终态会绕过模型流和工具响应的消费追踪器。

独立 Rust 复现保存一个尚未读取任何 item 的 ToolResponse 并返回下一批任务，观察结果：

```text
unconsumed stream state: Succeeded { output: ContextOutput { consumed: false } }
stream state after parent cancellation: Succeeded { output: ContextOutput { consumed: false } }
stream consumption still blocked after cancellation: true
```

父取消不能覆盖已经提交的成功状态，响应的取消订阅也无法退出。此缺陷影响自定义 Plan 保留/异步消费流的路径；当前 SingleAgent 同步消费路径及现有 RT::exec 流测试可以正常通过。

建议由 `TrackedModelStream` / `ToolCompletion` 在 EOF、Completed、错误或最后消费句柄丢弃时独占提交任务终态；对其他响应协议显式区分处理。补充 Plan 异步回调保留响应时的取消验收。

## 2. P2：部分异步 runtime 缺少 Started

置信度：10/10。位置：`crates/fae-agent/src/runtime/runtime.rs:223-227`，以及 Memory/Session/Workflow/Mcp/Compression 的 spawn 实现。
关联要求：第 6.1/6.2 节，K1/K3。

```rust
let _ = ctx.append_stack(ContextStackDraft::new(
    meta.clone(),
    ContextStackPhase::Dispatched,
));
let result = self.inner.spawn(req).await;
```

通用 wrapper 只记录派发；Plan、Model、Tools 额外记录开始，而其他上述内置异步 runtime 未补写 Started。独立复现通过 Engine 的 RT::spawn 执行 Memory Query 并等待真实回调，得到：

```text
async memory stack phases: [Dispatched, Succeeded]
```

`EngineContext::append_stack` 将 Dispatched 用作开始时间兜底，所以终态关联派发记录，elapsed_ms 包括排队时间，无法表达实际开始执行及纯执行耗时。

建议在 runtime 接受任务并开始执行时记录 Started；终态关联真实开始记录。端到端耗时若有需要，应单独表达。补充这些异步 runtime 的阶段及父记录关系验收。

## 本次验证

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo check --workspace --all-targets --locked` | 通过 |
| fae-agent lib 测试 | 90 通过 |
| fae-engine lib 测试 | 61 通过，按方案跳过 2 项真实模型联网测试 |
| `git diff --check` | 通过 |
| 独立 Rust 流式回调复现 | 确认提前成功及父取消后消费超时 |
| 独立 Rust 异步 Memory 栈复现 | 确认缺少 Started |

工作区编译仅有 async-openai 原有 Assistants API 弃用警告。本次未修改被审查的 Rust 实现和原方案文档。中间复现工程及审查数据位于 `/tmp/free-agent-engine_context_review/`。

进程组终止、外部副作用恢复和历史 master 数据迁移仍按原方案列为后续工作；本次不据此认定额外缺陷。静态审查及当前测试没有证明所有可能的扩展 runtime 均满足协议。
