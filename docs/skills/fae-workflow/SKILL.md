---
name: "fae-workflow"
description: "Creates and runs FAE workflows from JSON configuration. Invoke when defining workflow nodes, actions, conditions, templates, nesting, loading, or troubleshooting."
---

# FAE Workflow

使用本 Skill 通过 JSON 配置创建、运行和排查 FAE Workflow。

## 核心原则

- 默认交付 Workflow JSON，不编写 Rust Builder 代码。
- 配置文件放在 `${FAE_HOST:-~/.fae}/workflows/<workflow-id>.json`。
- 文件名、配置中的 `id`、运行命令中的 workflow ID 必须一致。
- 优先复用 `fae` 已注册的 `tool`、`workflow`、`single_agent`、`session` 和
  `python` action。
- 仅当用户需要新的 `custom` action，或要把 Workflow 嵌入其他 Rust 应用时，才修改代码和
  runtime。

## 标准流程

1. 明确 workflow 的输入 JSON、最终输出和可能产生的副作用。
2. 选择最简单的图：串行优先；只有独立任务才并行；重试才使用 `loop`。
3. 创建 `${FAE_HOST:-~/.fae}/workflows/<workflow-id>.json`。
4. 使用 `{$input...}` 和 `{$node_id...}` 在节点间传值，并显式配置 end `output`。
5. 先用 `jq empty <file>` 检查 JSON 语法，再运行 workflow 触发完整图校验。
6. 用内联 JSON 或 `@input.json` 执行：

```bash
fae workflow <workflow-id> --input '{"key":"value"}'
fae workflow <workflow-id> --input @input.json
```

仓库内开发时可使用：

```bash
cargo run -p fae -- workflow <workflow-id> --input @input.json
```

自定义目录使用全局参数：

```bash
fae --fae-home /path/to/fae-home workflow <workflow-id> --input @input.json
```

## 最小配置

保存为 `~/.fae/workflows/echo-input.json`：

```json
{
  "version": 1,
  "id": "echo-input",
  "nodes": {
    "start": {
      "type": "start",
      "next": ["end"]
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

运行：

```bash
fae workflow echo-input --input '{"message":"hello"}'
```

## 配置约束

- 必须恰好有一个 `start` 和一个 `end`。
- 所有节点都必须能从 start 到达，并存在到 end 的路径。
- `next`、`on_true`、`on_false` 中的节点必须存在，且不能指回 start。
- 普通环非法；循环必须使用 `loop`，循环体必须返回该 loop。
- `max_iterations` 必须大于零。
- fan-out 或多前驱汇合会进入 DAG 执行；同一 workflow 中不能再包含 loop。
- 并行节点不要读取 `{$last}`；应通过明确的节点 ID 引用输出。
- end 应显式配置 `output`，避免图调整改变隐式返回值。

## 模板规则

| 模板 | 含义 |
| --- | --- |
| `{$input}` | 完整 workflow 输入 |
| `{$input.path}` | 输入对象字段 |
| `{$node_id.result}` | 已完成节点的输出字段 |
| `{$last.result}` | 顺序流程中最近 action 的输出 |
| `{$loop.retry.iteration}` | loop 当前迭代次数，从 1 开始 |

完整字符串仅包含一个模板时保留 JSON 类型；模板嵌入普通文本时结果是字符串：

```json
{
  "number": "{$input.count}",
  "message": "count={$input.count}"
}
```

## 按需阅读

- 创建或修改配置时，先读
  [references/workflow-api.md](references/workflow-api.md)，确认节点、action 和条件的 JSON
  字段。
- 需要串行、条件、并行、循环、父子流程或 Agent 节点时，读
  [references/recipes.md](references/recipes.md)，从完整 JSON 配方开始修改。

## 排错顺序

1. 确认实际 `FAE_HOST`、文件路径、文件名和 `id`。
2. 用 `jq empty` 排除 JSON 语法错误。
3. 检查 start/end 唯一性、目标节点拼写、可达性和环。
4. 检查模板引用的节点是否一定先完成，字段是否真实存在。
5. 检查 action 依赖：工具名、Agent 配置、子 workflow、Python 解释器或自定义 runtime。
6. 检查并行汇合的每个活跃分支是否都能到达汇合节点。

不要通过改 Rust 绕过配置校验。若现有 action 无法表达需求，再明确新增 runtime 的必要性。
