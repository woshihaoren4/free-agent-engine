# FAE Workflow JSON 配置

本文是磁盘 Workflow 配置的字段手册。默认通过 `fae workflow` 加载配置，不需要在 Rust 中构造
`WorkflowMetadataBuilder` 或注册 runtime。

## 1. 文件位置与运行

默认文件：

```text
${FAE_HOST:-~/.fae}/workflows/<workflow-id>.json
```

自定义 home：

```bash
fae --fae-home /path/to/home workflow <workflow-id> --input @input.json
```

默认 home：

```bash
fae workflow <workflow-id> --input '{"key":"value"}'
```

加载规则：

- `<workflow-id>.json` 的文件名和文件内 `id` 必须与命令参数相同。
- ID 必须是单个非空路径组件，不能包含目录。
- `FAE_HOST` 支持 `~` 和 `~/...`。
- `--input` 必须是合法 JSON 值；`@path` 表示从文件读取 JSON。
- 配置在执行前反序列化并完成整图校验。

## 2. 顶层结构

```json
{
  "version": 1,
  "id": "workflow-id",
  "nodes": {
    "start": {
      "type": "start",
      "next": ["work"]
    },
    "work": {
      "type": "execute",
      "action": {
        "type": "python",
        "code": "result = arguments",
        "arguments": "{$input}"
      },
      "next": ["end"]
    },
    "end": {
      "type": "end",
      "output": "{$work}"
    }
  }
}
```

| 字段 | 要求 |
| --- | --- |
| `version` | 当前为 `1`；省略时也按 `1` 解析 |
| `id` | workflow ID，必须与文件名和运行参数一致 |
| `nodes` | 以节点 ID 为 key 的对象 |

目标字段统一写成字符串数组。读取旧配置时，`start.next`、`execute.next`、`decision.on_true` 和
`decision.on_false` 也兼容单个字符串，但新配置应使用数组。

## 3. 节点

### Start

一个目标表示串行入口，多个目标表示并行 fan-out：

```json
{
  "type": "start",
  "next": ["left", "right"]
}
```

start 的输出就是完整 workflow 输入。

### Execute

执行一个 action，保存其返回值为该节点输出，再激活 `next`：

```json
{
  "type": "execute",
  "action": {
    "type": "tool",
    "tool_name": "read_file",
    "arguments": {
      "path": "{$input.path}"
    }
  },
  "next": ["end"]
}
```

多个 `next` 表示 action 完成后 fan-out。

### Decision

计算条件并选择一组后继节点；节点输出为布尔值：

```json
{
  "type": "decision",
  "condition": {
    "type": "compare",
    "left": "{$input.score}",
    "op": "ge",
    "right": 80
  },
  "on_true": ["approved"],
  "on_false": ["rejected"]
}
```

每个分支都可以包含多个目标。

### Loop

条件为真时进入 `body`，为假时进入 `next`：

```json
{
  "type": "loop",
  "condition": {
    "type": "compare",
    "left": "{$last.remaining}",
    "op": "gt",
    "right": 0
  },
  "body": "decrement",
  "next": "end",
  "max_iterations": 10
}
```

- 循环体必须有路径返回当前 loop 节点。
- `max_iterations` 省略时为 `100`，且必须大于零。
- 条件持续为真并达到上限时，workflow 失败。
- loop 不能与 fan-out 或多前驱汇合出现在同一个 workflow。

### End

结束 workflow，并解析显式输出：

```json
{
  "type": "end",
  "output": {
    "status": "ok",
    "result": "{$work}"
  }
}
```

省略 `output` 时，串行流程返回最近 action 输出；没有 action 时返回 workflow 输入。DAG 会返回
直接活跃前驱的输出。生产配置应显式设置 `output`。

`parallel_start` 和 `join_end` 是兼容节点类型，但普通 `start` 和 `end` 已能表达 fan-out 与
join，配置中优先使用后者。

## 4. Conditions

### Truthy

```json
{
  "type": "truthy",
  "value": "{$input.enabled}"
}
```

`null`、`false`、数字零、空字符串、空数组和空对象为假。

### Exists

```json
{
  "type": "exists",
  "value": "{$optional_node.value}"
}
```

只判断引用能否解析。后续节点仍不能无条件读取一个可能不存在的分支输出。

### Compare

```json
{
  "type": "compare",
  "left": "{$input.count}",
  "op": "ge",
  "right": 3
}
```

`op` 支持 `eq`、`ne`、`gt`、`ge`、`lt`、`le`。有序比较只接受两个数字或两个字符串；
`eq` 和 `ne` 使用 JSON 值相等。

## 5. Actions

### Tool

调用 `fae` 已注册的工具：

```json
{
  "type": "tool",
  "tool_name": "read_file",
  "arguments": {
    "path": "{$input.path}",
    "max_bytes": 8192
  }
}
```

工具完成输出若为合法 JSON，会保存为 JSON 值；否则保存为字符串。工具名必须可被当前
`ToolsRuntime` 路由。

### Workflow

调用同一 home 下的另一个配置式 workflow：

```json
{
  "type": "workflow",
  "workflow_id": "validate-order",
  "input": {
    "order_id": "{$input.order.id}"
  }
}
```

子 workflow 位于同一个 `workflows/` 目录，父节点输出是子 workflow 的最终 JSON。可通过父子
workflow 隔离 loop 和并行 DAG。

### Single Agent

按 Agent ID 加载 `${FAE_HOST}/agents/<id>_config.json` 和 `<id>_prompt.txt`：

```json
{
  "type": "single_agent",
  "source": {
    "agent_id": "reviewer"
  },
  "input": "Review this content:\n{$input.content}"
}
```

也可显式指定两个路径：

```json
{
  "type": "single_agent",
  "source": {
    "paths": {
      "config": "/absolute/path/reviewer_config.json",
      "prompt": "/absolute/path/reviewer_prompt.txt"
    }
  },
  "input": "{$input.request}"
}
```

Agent 最终文本是节点输出。模型、工具、Skill 和 MCP 事件会进入 workflow 的同一个 session。

### Python

`fae` 默认注册 `workflow.python`：

```json
{
  "type": "python",
  "code": "result = {\"total\": arguments[\"left\"] + arguments[\"right\"]}",
  "arguments": {
    "left": "{$input.left}",
    "right": "{$input.right}"
  }
}
```

执行环境约定：

- 系统必须能运行 `python3`。
- `arguments` 是传给脚本的 JSON 值。
- 脚本必须给变量 `result` 赋一个可 JSON 序列化的值。
- `task_type` 可省略，默认是 `workflow.python`。

### Session

查询会话历史：

```json
{
  "type": "session",
  "request": {
    "Query": {
      "user": "alice",
      "session_id": "session-1",
      "limit": 20,
      "offset": null
    }
  }
}
```

追加消息：

```json
{
  "type": "session",
  "request": {
    "Add": {
      "user": "alice",
      "session_id": "session-1",
      "messages": [
        {
          "role": "user",
          "content": "{$input.message}"
        },
        {
          "role": "assistant",
          "content": "{$agent_reply}"
        }
      ]
    }
  }
}
```

删除会话使用 `{"Delete":{"user":"alice","session_id":"session-1"}}`。`SessionRequest`
当前使用区分大小写的 `Query`、`Add`、`Delete` 外部标签。

### Custom

```json
{
  "type": "custom",
  "task_type": "my.workflow.action",
  "request": {
    "id": "{$input.id}"
  }
}
```

`custom` 不会自动执行。只有宿主已经注册完全匹配 `task_type` 的 runtime 时才能使用。标准
`fae` CLI 不会为项目自定义 task type 自动注册 runtime。

## 6. 值模板

模板会递归处理对象和数组的 value，不处理对象 key。

| 表达式 | 含义 |
| --- | --- |
| `{$input}` | 完整输入 |
| `{$input.user.name}` | 输入对象字段 |
| `{$input.items.0}` | 数组元素 |
| `{$read.content}` | `read` 节点输出字段 |
| `{$last.remaining}` | 顺序执行器中最近 action 的输出 |
| `{$loop.retry.iteration}` | `retry` loop 当前次数 |

精确引用保留原 JSON 类型：

```json
{
  "count": "{$input.count}"
}
```

引用嵌入文本后得到字符串：

```json
{
  "message": "count={$input.count}"
}
```

字段缺失、数组越界、从标量继续取字段、读取尚未完成的节点都会失败。并行分支只能读取确定已
完成的共同祖先；汇合后的节点可以读取所有已选择分支。

## 7. 图校验

配置加载时会验证：

- start 和 end 各有且仅有一个。
- 每个节点都可从 start 到达，并且能到达 end。
- 所有目标节点都存在，且边不返回 start。
- 普通环不存在；loop 的循环体会返回该 loop。
- loop 上限有效，且没有与 DAG 并行结构混用。

先检查 JSON 语法：

```bash
jq empty "${FAE_HOST:-$HOME/.fae}/workflows/<workflow-id>.json"
```

再通过 `fae workflow` 加载并运行。当前 CLI 没有独立的 validate 子命令，因此对有副作用的
workflow，应先用无副作用的测试输入或隔离的 `--fae-home` 验证。

## 8. 何时需要代码

仅在以下情况进入 Rust 实现：

- 新增 `custom` action 对应的 runtime。
- 需要把 workflow 执行能力嵌入其他 Rust 应用。
- 需要不同于 `fae` CLI 默认值的 runtime、工具或事件处理。

这类任务才需要关注 `WorkflowRuntime`、`WorkflowPlanBuilder`、
`FAEWorkflowMetadataLoader` 和 `WorkflowEnv`。单纯创建或修改 workflow 不需要改代码。
