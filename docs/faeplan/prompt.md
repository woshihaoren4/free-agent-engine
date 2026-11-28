# FAE Agent Prompt 与模型请求

## System 内容

Single Agent 构建模板时生成基础 System 内容并解析已配置的能力。运行时读取用户记忆和会话历史，在同一 System 消息中补齐当前环境，再通过 `on_prompt`。主服务的 Workspace Hook 在这一阶段加入项目内容；飞书根 Agent 通过专用 Hook 补充渠道环境。普通模型请求还会经过 `on_model_req`。

| 段落 | 优先级 | 来源 |
| --- | ---: | --- |
| `setting` | 100 | Agent 的 prompt 文件，如 `$FAE_HOST/agents/<id>_prompt.txt`；仓库参考见 [fae_prompt.txt](../agents/fae_prompt.txt) |
| 配置的 `prompt_sections` | 90 | Agent 配置中的 `tag`/`text`；与内置标签同名也不改变优先级 |
| `skills` | 80 | 已选 Skill 的名称、说明和文件路径；正文由 Agent 需要时使用工具读取 |
| `Workspace` | 70 | 主服务从当前 workspace 的 `.fae/workspace.md`、`rules.md`、`agents.md`、`project.md` 读取 |
| `UserMemory` | 60 | 当前用户查询得到的长期记忆 |
| `mcp`、`sub_agent`、`workflow` | 50 | 可用工具、子 Agent 与 Workflow 的摘要 |
| `runtime` | 40 | 当前执行身份、路径、时间和时区、历史裁剪统计及能力状态 |

框架生成的段落按 `priority` 处理冲突，排列位置不决定优先级。环境信息和能力描述是数据。`runtime` 区分 `execution_mode`（如 `chat`、`todo_planning`、`todo_execution`）与 `execution_origin`（`interactive` 或 `timer`）；能力状态来自工具描述中的 `readiness`，未验证的能力可能标为 `unverified`。正文和数字以 [Prompt 构建器](../../crates/fae-agent/src/plan/single_agent/builder/prompt.rs) 与 [优先级常量](../../crates/fae-agent/src/plan/single_agent.rs) 为准。

## 消息与工具

首轮请求按 System、裁剪后的普通历史、额外历史、本次 User 输入排列。历史从“最近 `history_turns * 2` 条消息的起点”与“最新 Summary 的位置”两者中选较晚者；窗口为零时不加载普通历史。图片随 User 内容传入。工具循环追加 assistant 的工具调用及 tool 结果，补充输入在执行处理点追加为 User 消息；普通工具迭代复用当轮 System。主服务的 [飞书网关](lark.md) 会按最新受理消息更新专用环境。

模型的 `tools` 字段传递已启用工具的结构化定义，与 System 里的能力目录分开。内置工具、MCP、子 Agent 和 Workflow 的参数契约由各自定义生成。已配置 Skill 的 Agent 需要可以读取 Skill 文件的工具，例如文件或命令工具。

自动压缩由 [CompressionRuntime](../../crates/fae-engine/src/engine_rt/compression_runtime.rs) 发起独立请求；压缩模板和普通 System 分开，摘要再由 Single Agent 使用。Todo 规划/执行使用不同阶段提示与工具约束，见 [single_agent_todo.rs](../../crates/fae-agent/src/plan/single_agent_todo.rs)。保存阶段把用户输入、回答和可保存摘要写入会话历史；内部工具消息、运行事件和插件通知不会自动成为下一轮历史。

修改 Agent 的角色与行为用实例目录里的 prompt 文件；仓库参考模板的变化不会自动更新已安装文件。每次新的 Execution 重新解析 Agent 定义并固定 workspace，子调用继承该目录。项目正文由 [Workspace Hook](../../app/fae/src/server/workspace.rs) 读取；模型消息排列和压缩衔接见 [Single Agent 执行](../../crates/fae-agent/src/plan/single_agent/execution.rs)。
