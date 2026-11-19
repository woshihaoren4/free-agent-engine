# Plugin 示例

三个独立目录插件，均为 `direct` 模式；`plugin.config` 声明挂载位置，
`plugin.py` 实现类。宿主已包含 Python 插件支持。

| 目录 / 加载 ID | 接口 | 可见效果 |
| --- | --- | --- |
| `single_agent` | SingleAgentHook | 查询 ctx/scope/state，session 发提示消息，记录日志，清理输入及补充 prompt |
| `table_image` | ToolsHook | Agent 调用 `fae_table_image__generate`，由 Python/Pillow 生成真实 PNG |
| `echo_model` | ModelHook | 最后一条用户文本前添加 `[model-plugin] `，支持完整及逐块流式响应 |

## 安装和加载

以下命令在仓库根目录运行，`fae` 使用本次编译的版本。也可先执行
`cargo build -p fae`，再将下面的 `fae` 替换为 `./target/debug/fae`。
SDK 和 Pillow 必须安装到宿主实际使用的解释器。

```bash
python3 -m venv /tmp/fae-plugin-demo-venv
/tmp/fae-plugin-demo-venv/bin/python -m pip install -e ./python/fae-plugin-py
/tmp/fae-plugin-demo-venv/bin/python -m pip install -r examples/plugin/table_image/requirements.txt
export FAE_PLUGIN_PYTHON=/tmp/fae-plugin-demo-venv/bin/python

# 独立示例 home，不覆盖日常使用的 ~/.fae。
export FAE_HOST=/tmp/fae-plugin-demo
mkdir -p "$FAE_HOST/plugins" "$FAE_HOST/agents"
cp -R examples/plugin/single_agent examples/plugin/table_image examples/plugin/echo_model "$FAE_HOST/plugins/"
cp examples/plugin/agents/* "$FAE_HOST/agents/"
fae server start --port 26667
fae plugin single_agent
fae plugin table_image
fae plugin echo_model
fae plugin --show '*'
```

修改示例后，重新复制对应目录并执行 `fae plugin <id>` 即可重加载。
修改 Agent 配置后，使用新 session ID 运行，避免已存在会话的冻结配置。
环境变量需在服务启动前设置；已经运行的服务不会继承客户端的新环境变量。

## 1. Single Agent：ctx、session 和日志

```bash
fae agent --agent-id plugin_demo --session-id hook-demo "  你好，插件  "
```

首先看到 `[single-agent-plugin] agent=plugin_demo，已收到请求，开始处理。`，
随后看到模型响应 `[model-plugin] 你好，插件`。`on_query` 去除了两端空白，
`on_prompt` 补充了一句规则；没有重写的方法继续默认透传。

代码演示 `ctx.agent_id/run_id/user_id`、`await ctx.scope()`、
`ctx.state().current()` 和订阅关闭，以及 `ctx.session.send()` 的消息回执。
session 可能为空，示例先检查再发送。提示消息不进入模型历史。
日志使用 `logging` 写入 stderr，可在 `$FAE_HOST/server/server.log` 中查看
`[single-agent-plugin]` 的上下文、发送回执及 `on_save` 记录。

## 2. Tool：生成表格图片并挂给 Agent

`table_demo_config.json` 中的挂载配置为：

```json
{
  "model": {"model": "gpt-4o-mini"},
  "tools": ["fae_table_image__generate"]
}
```

加载插件提供工具实现；Agent 的 `tools` 数组选择模型可调用的工具。
已有 Agent 也可把该工具名加入自己的 `tools`。
表格 Agent 使用支持 tool calling 的真实模型，需要配置其模型名和
`OPENAI_API_KEY`（以及所用 OpenAI 兼容服务的配置），并在启动服务前设置凭据。
已有模型配置可直接用于这个 Agent。

```bash
fae agent --agent-id table_demo --session-id table-demo --workspace /tmp \
  '请调用表格工具，path 为 plugin-table.png，x 为 ["Jan","Feb","Mar"]，y 为 [120,180,150]，title 为 Sales。'
```

工具参数示例：

```json
{
  "path": "/tmp/plugin-table.png",
  "x": ["Jan", "Feb", "Mar"],
  "y": [120, 180, 150],
  "title": "Sales",
  "x_label": "Month",
  "y_label": "Revenue"
}
```

x/y 必须等长，每对数据生成一行，最多 100 行。`path` 只接受 `.png`，
相对路径基于实际 Agent workspace，父目录自动创建，同名文件会覆盖。
工具返回 JSON 字符串，包含绝对路径、行数及图片宽高，并发送一条 session 提示。
返回路径指向运行 FAE 服务的机器；示例不会自动上传文件。
Pillow 使用内置英文字体；中文标题/内容需传入支持中文的 TTF/OTF/TTC
绝对路径 `font_path`，例如安装 Noto Sans CJK 后的实际字体路径。

只验证 Python 绘图、不调用模型时，可在仓库根目录运行：

```bash
"$FAE_PLUGIN_PYTHON" - <<'PY'
import runpy
plugin = runpy.run_path("examples/plugin/table_image/plugin.py")
result = plugin["render_table"]({
    "path": "/tmp/plugin-table.png",
    "x": ["Jan", "Feb", "Mar"],
    "y": [120, 180, 150],
    "title": "Sales",
}, None)
print(result)
PY
```

## 3. Model：带标识回显

```bash
fae agent --agent-id model_demo --session-id model-demo "hello model"
```

预期响应为 `[model-plugin] hello model`。这个模型本地运行，不需要模型凭据；
`EchoModel.match_model(ctx, model)` 判断名称是否为 `plugin:echo_model/default`，
挂载配置仅包含 `point` 和 `implementation`，不声明模型名称。
可以改判断方法来支持多个名称或一个前缀，并同步调整 Agent 的 `model.model`。
Agent 正常流式调用它，`request.stream=False` 时也返回合法完整响应。
它只回显最后一条用户文本，不执行推理、图片识别或 tool calling。
因此表格 Agent 应使用真实模型，而 `plugin_demo` 与 `model_demo` 可直接
用它体验 Hook 和模型挂载。

体验完后执行 `fae server stop` 停止这个独立 home 的服务。
完整 SDK API 见 [Python SDK](../../python/fae-plugin-py/README.md)。

更新已有模型示例时，重新复制 `echo_model` 目录并执行 `fae plugin echo_model`。
旧版本 plugin.config 中的 model 字段已移除；判断逻辑改在 match_model 中实现。
