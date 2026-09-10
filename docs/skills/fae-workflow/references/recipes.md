# FAE Workflow 配置配方

以下配方均以 `${FAE_HOST:-~/.fae}/workflows/` 为根目录。复制完整 JSON 后，修改 ID、节点和业务
参数，不要改成 Rust Builder。

## 1. 串行 Tool Workflow

保存为 `read-file.json`：

```json
{
  "version": 1,
  "id": "read-file",
  "nodes": {
    "start": {
      "type": "start",
      "next": ["read"]
    },
    "read": {
      "type": "execute",
      "action": {
        "type": "tool",
        "tool_name": "read_file",
        "arguments": {
          "path": "{$input.path}",
          "max_bytes": 8192
        }
      },
      "next": ["end"]
    },
    "end": {
      "type": "end",
      "output": {
        "path": "{$read.path}",
        "content": "{$read.content}",
        "truncated": "{$read.truncated}"
      }
    }
  }
}
```

运行：

```bash
fae workflow read-file --input '{"path":"Cargo.toml"}'
```

适合步骤固定、后一步依赖前一步输出的流程。

## 2. 条件分支

保存为 `score-route.json`：

```json
{
  "version": 1,
  "id": "score-route",
  "nodes": {
    "start": {
      "type": "start",
      "next": ["route"]
    },
    "route": {
      "type": "decision",
      "condition": {
        "type": "compare",
        "left": "{$input.score}",
        "op": "ge",
        "right": 80
      },
      "on_true": ["approved"],
      "on_false": ["rejected"]
    },
    "approved": {
      "type": "execute",
      "action": {
        "type": "python",
        "code": "result = {\"approved\": True, \"reason\": \"score accepted\"}"
      },
      "next": ["end"]
    },
    "rejected": {
      "type": "execute",
      "action": {
        "type": "python",
        "code": "result = {\"approved\": False, \"reason\": \"score too low\"}"
      },
      "next": ["end"]
    },
    "end": {
      "type": "end"
    }
  }
}
```

```bash
fae workflow score-route --input '{"score":86}'
```

两个分支都输出相同结构，因此可以安全使用 end 的隐式分支结果。若分支输出结构不同，应在每个
分支后做归一化；不要在 end 中引用只会执行其中一个的节点。

## 3. 并行 Fan-out 与 Join

保存为 `parallel-inspect.json`：

```json
{
  "version": 1,
  "id": "parallel-inspect",
  "nodes": {
    "start": {
      "type": "start",
      "next": ["read_source", "read_manifest"]
    },
    "read_source": {
      "type": "execute",
      "action": {
        "type": "tool",
        "tool_name": "read_file",
        "arguments": {
          "path": "{$input.source_path}",
          "max_bytes": 16384
        }
      },
      "next": ["summarize"]
    },
    "read_manifest": {
      "type": "execute",
      "action": {
        "type": "tool",
        "tool_name": "read_file",
        "arguments": {
          "path": "{$input.manifest_path}",
          "max_bytes": 8192
        }
      },
      "next": ["summarize"]
    },
    "summarize": {
      "type": "execute",
      "action": {
        "type": "python",
        "code": "result = {\"source_bytes\": len(arguments[\"source\"]), \"manifest_bytes\": len(arguments[\"manifest\"])}",
        "arguments": {
          "source": "{$read_source.content}",
          "manifest": "{$read_manifest.content}"
        }
      },
      "next": ["end"]
    },
    "end": {
      "type": "end",
      "output": "{$summarize}"
    }
  }
}
```

```bash
fae workflow parallel-inspect --input \
  '{"source_path":"src/main.rs","manifest_path":"Cargo.toml"}'
```

`summarize` 有两个前驱，只有两个活跃分支都完成后才执行。并行分支中使用明确节点 ID，不使用
`{$last}`。

## 4. 有界循环

保存为 `bounded-countdown.json`：

```json
{
  "version": 1,
  "id": "bounded-countdown",
  "nodes": {
    "start": {
      "type": "start",
      "next": ["initialize"]
    },
    "initialize": {
      "type": "execute",
      "action": {
        "type": "python",
        "code": "result = {\"remaining\": arguments[\"rounds\"], \"completed\": 0}",
        "arguments": {
          "rounds": "{$input.rounds}"
        }
      },
      "next": ["retry"]
    },
    "retry": {
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
    },
    "decrement": {
      "type": "execute",
      "action": {
        "type": "python",
        "code": "result = {\"remaining\": arguments[\"remaining\"] - 1, \"completed\": arguments[\"iteration\"]}",
        "arguments": {
          "remaining": "{$last.remaining}",
          "iteration": "{$loop.retry.iteration}"
        }
      },
      "next": ["retry"]
    },
    "end": {
      "type": "end",
      "output": "{$last}"
    }
  }
}
```

```bash
fae workflow bounded-countdown --input '{"rounds":3}'
```

循环注意事项：

- 进入 loop 前先生成条件需要的状态。
- `max_iterations` 是故障保护，不能小于合法输入需要的最大次数。
- 循环体必须返回 loop 节点。
- 输入为零时 `{$loop.retry.iteration}` 不存在，因此 end 使用 `{$last}`，兼容零次循环。
- 同一配置不能同时使用 loop 和 fan-out；需要组合时拆成父子 workflow。

## 5. 父子 Workflow

先保存子流程 `validate-order.json`：

```json
{
  "version": 1,
  "id": "validate-order",
  "nodes": {
    "start": {
      "type": "start",
      "next": ["validate"]
    },
    "validate": {
      "type": "execute",
      "action": {
        "type": "python",
        "code": "result = {\"order_id\": arguments[\"order_id\"], \"valid\": bool(arguments[\"order_id\"])}",
        "arguments": {
          "order_id": "{$input.order_id}"
        }
      },
      "next": ["end"]
    },
    "end": {
      "type": "end",
      "output": "{$validate}"
    }
  }
}
```

再保存父流程 `process-order.json`：

```json
{
  "version": 1,
  "id": "process-order",
  "nodes": {
    "start": {
      "type": "start",
      "next": ["validate"]
    },
    "validate": {
      "type": "execute",
      "action": {
        "type": "workflow",
        "workflow_id": "validate-order",
        "input": {
          "order_id": "{$input.order.id}"
        }
      },
      "next": ["end"]
    },
    "end": {
      "type": "end",
      "output": {
        "order": "{$validate}"
      }
    }
  }
}
```

```bash
fae workflow process-order --input '{"order":{"id":"order-42"}}'
```

两个文件必须位于同一个 `FAE_HOST/workflows/`。父子配置适合复用子流程，以及隔离不能共存的
并行图和 loop。

## 6. Single Agent 节点

前提是 `${FAE_HOST}/agents/reviewer_config.json` 和 `reviewer_prompt.txt` 已存在。保存为
`agent-review.json`：

```json
{
  "version": 1,
  "id": "agent-review",
  "nodes": {
    "start": {
      "type": "start",
      "next": ["review"]
    },
    "review": {
      "type": "execute",
      "action": {
        "type": "single_agent",
        "source": {
          "agent_id": "reviewer"
        },
        "input": "Review the following content and return concise findings:\n{$input.content}"
      },
      "next": ["end"]
    },
    "end": {
      "type": "end",
      "output": {
        "review": "{$review}"
      }
    }
  }
}
```

```bash
fae workflow agent-review --input @review-input.json
```

Agent 的最终文本是 `review` 节点输出。其模型和工具事件会实时显示在 workflow TUI 中。

## 7. 读取并更新 Session

保存为 `session-reply.json`：

```json
{
  "version": 1,
  "id": "session-reply",
  "nodes": {
    "start": {
      "type": "start",
      "next": ["history"]
    },
    "history": {
      "type": "execute",
      "action": {
        "type": "session",
        "request": {
          "Query": {
            "user": "{$input.user}",
            "session_id": "{$input.session_id}",
            "limit": 20,
            "offset": null
          }
        }
      },
      "next": ["reply"]
    },
    "reply": {
      "type": "execute",
      "action": {
        "type": "single_agent",
        "source": {
          "agent_id": "fae"
        },
        "input": {
          "history": "{$history}",
          "message": "{$input.message}"
        }
      },
      "next": ["save"]
    },
    "save": {
      "type": "execute",
      "action": {
        "type": "session",
        "request": {
          "Add": {
            "user": "{$input.user}",
            "session_id": "{$input.session_id}",
            "messages": [
              {
                "role": "user",
                "content": "{$input.message}"
              },
              {
                "role": "assistant",
                "content": "{$reply}"
              }
            ]
          }
        }
      },
      "next": ["end"]
    },
    "end": {
      "type": "end",
      "output": "{$reply}"
    }
  }
}
```

`single_agent.input` 可以是任意 JSON 值，但对象最终会按 JSON 文本交给 Agent。需要精确控制
提示词时，使用一个字符串模板。

## 8. 使用隔离目录测试

准备目录：

```text
/tmp/fae-workflow-test/
├── agents/
├── mcp/
├── skills/
└── workflows/
    └── example.json
```

执行：

```bash
jq empty /tmp/fae-workflow-test/workflows/example.json
fae --fae-home /tmp/fae-workflow-test workflow example --input @input.json
```

隔离目录能避免读取用户真实配置，也能限制 workflow 测试产生的 session 或文件副作用。若配置
使用 Agent、Skill 或 MCP，必须同时把对应依赖放入隔离目录。

## 9. 排错清单

### 找不到 Workflow

- 检查 `FAE_HOST` 或 `--fae-home` 是否指向预期目录。
- 检查文件是否位于 `<home>/workflows/<id>.json`。
- 检查文件名、配置 `id` 和命令参数是否完全一致。

### 配置校验失败

- 检查 start/end 是否各一个。
- 检查所有目标节点拼写。
- 检查是否存在不可达节点或无法到达 end 的节点。
- 检查普通环是否应改成 `loop`。
- 检查 fan-out/DAG 是否混入 loop。

### Action 无法执行

- Tool：确认 `tool_name` 是已注册工具。
- Workflow：确认子配置位于同一 home 且 ID 一致。
- Single Agent：确认 Agent config、prompt 和模型环境变量可用。
- Python：确认 `python3` 可执行，脚本给 `result` 赋值且结果可 JSON 序列化。
- Custom：标准 CLI 不会自动注册项目自定义 runtime。

### 模板解析失败

- 被引用节点是否一定在当前节点之前完成。
- 是否引用了未选中的条件分支。
- 对象字段和数组索引是否存在。
- 需要数字或对象时，是否误把模板嵌入普通字符串。
- 并行 DAG 中是否错误使用 `{$last}`。

### 并行流程卡住

- 每个活跃分支是否都能到达汇合节点。
- 汇合节点是否列为每个分支的后继。
- 汇合后的模板是否只读取确定会执行的分支。
