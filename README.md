# Free Agent Engine

Free Agent Engine（FAE）是一个用 Rust 编写的多智能体运行框架。它提供文件系统驱动的 Agent 配置、OpenAI 兼容模型执行器、内置工具、Skill 加载、MCP 工具接入、子 Agent 调度、定时任务运行时，以及一个可直接使用的命令行程序 `fae`。

## 功能概览

- Rust workspace：核心抽象、运行时和 CLI 拆分为独立 crate。
- OpenAI 兼容模型通道：默认使用 Chat Completions 风格接口，支持自定义 API base。
- 文件型 Agent 配置：Agent 从 `$FAE_HOME/<workspace>/<agent_id>/config.json` 加载。
- 流式会话：支持 CLI 和 Rust API 的 streaming chat。
- 内置工具：文件读写、目录列表、命令执行、HTTP 请求、Python 执行、patch、todo、Ark Web Search、定时任务、子 Agent 任务。
- Skill 系统：从 `$FAE_HOME/skills/<skill_name>/SKILL.md` 加载技能元信息。
- MCP 接入：支持本地 stdio MCP server 和远程 HTTP JSON-RPC MCP server。
- 多 Agent 协作：父 Agent 可通过 `sub_agents` 和 `agent_exec_task` 调用专家 Agent。


## 快速开始

```bash
export OPENAI_API_KEY="sk-..."
export OPENAI_API_URL="https://api.openai.com/v1"
export FAE_DEFAULT_MODEL="gpt-xxx"

cargo run -p fae -- init
cargo run -p fae -- agent
cargo run -p fae -- agent--chat
```

`fae init` 会创建 `$FAE_HOME`，默认是 `~/.fae`，并初始化 prompt、skills、mcp 配置和默认 Agent。

## 环境变量

| 变量 | 说明 |
| --- | --- |
| `OPENAI_API_KEY` | 模型 API key。 |
| `OPENAI_API_URL` | FAE 模型执行器优先读取的 API base，例如 `https://api.openai.com/v1`。 |
| `OPENAI_BASE_URL` | vendored `async-openai` 客户端支持的 API base；当未设置 `OPENAI_API_URL` 时可用。 |
| `FAE_DEFAULT_MODEL` | FAE 默认模型名，优先级高于 `OPENAI_DEFAULT_MODEL`。 |
| `OPENAI_DEFAULT_MODEL` | 默认模型名，例如 `gpt-4o`。 |
| `FAE_HOME` | FAE 运行时根目录，默认是 `~/.fae`。 |
| `ARK_WEB_SEARCH_APIKEY` | Volcano Engine Ark Web Search 工具的 API key。 |

## CLI 用法

CLI 的 Cargo package 和二进制名都是 `fae`。

```bash
cargo run -p fae -- --help
cargo run -p fae -- --ws main init
cargo run -p fae -- --ws main agent
cargo run -p fae -- --ws main agent --id fae-assistant --chat
cargo run -p fae -- --ws main agent --id fae-assistant --user master --history
cargo run -p fae -- --ws main uninstall
```

也可以安装到本地 Cargo bin：

```bash
cargo install --path app/fae-ctl
fae --ws main agent
```

默认 Agent：

| Agent ID | 角色 |
| --- | --- |
| `fae-assistant` | 任务协调助手，负责理解、拆解、分配和监督任务。 |
| `fae-aicoding` | 编程助手，用于项目开发、脚本编写、错误修复和工程实现。 |
| `fae-claw` | 电脑管家，用于系统操作、文件处理、办公任务和通用问答。 |
| `fae-aitest` | 测试助手，用于审查实现、设计测试和执行验证。 |

聊天界面支持：

- `/exit`：退出会话。
- `/reset`：重置当前会话。
- `ctrl+j`：输入换行。
- `ctrl+c`：中止当前响应；无响应时退出 CLI。

## 运行时目录

FAE 的运行时数据默认放在 `~/.fae`，可通过 `FAE_HOME` 改写：

```text
$FAE_HOME/
├── prompt/                         # 共享 prompt
├── skills/<skill_name>/SKILL.md    # skill 文件
├── mcp/*.json                      # MCP server 配置
└── <workspace>/<agent_id>/
    ├── config.json                 # Agent 配置
    └── ...                         # prompt、会话、记忆等运行数据
```

`fae --ws <name> uninstall` 只删除对应 workspace 目录，不删除全局 `prompt`、`skills` 和 `mcp` 目录。

## Agent 配置

Agent 配置文件位于 `$FAE_HOME/<workspace>/<agent_id>/config.json`。核心字段如下：

```json
{
  "name": "Researcher",
  "description": "Searches, reads, and summarizes project information.",
  "model": {
    "model": "gpt-4o",
    "channel": "OpenAI-Compatible API",
    "max_chat_history_round": 20,
    "reasoning_effort": 2,
    "max_completion_tokens": null,
    "min_compact_window_size": 65536,
    "temperature": 1.0,
    "top_p": 1.0
  },
  "prompt_dir": "system.txt",
  "tools": [
    { "name": "read_file", "channel": "default" },
    { "name": "send_http_request", "channel": "default" }
  ],
  "skills": [
    { "name": "fae", "channel": "default" }
  ],
  "mcp_servers": [],
  "sub_agents": [],
  "custom": {}
}
```

`prompt_dir` 可以是绝对路径，也可以是相对于 Agent 目录的路径。`tools`、`skills`、`mcp_servers` 只会暴露当前 Agent 配置中启用的能力。

内置工具名：

```text
read_file
write_file
list_directory
execute_command
execute_python
send_http_request
apply_patch
todo_write
ark_web_search
scheduled_execution
agent_exec_task
```

## Skill 和 MCP

Skill 默认从 `$FAE_HOME/skills/<skill_name>/SKILL.md` 加载。仓库内可分发的 skill 放在 `docs/skills/`；新增或调整分发文件后，可重新生成下载清单：

```bash
cd docs
python3 generate_site.py
```

MCP 配置会从 `$FAE_HOME/mcp/*.json` 扫描，格式如下：

```json
{
  "mcpServers": {
    "remote_name": {
      "url": "https://example.com/mcp",
      "headers": {
        "Authorization": "Bearer <token>"
      }
    },
    "local_name": {
      "command": "npx",
      "args": ["-y", "@vendor/mcp-server"],
      "env": {
        "API_KEY": "<token>"
      }
    }
  }
}
```

在 Agent 的 `config.json` 中通过 `mcp_servers` 启用指定 server。MCP 工具名会带上 server 前缀，例如 `gaode__maps_text_search`。

## Rust API 示例

仓库提供两个 example：

```bash
cargo run -p examples --example basic
cargo run -p examples --example single_agent
```

最小用法：

```rust
use fae_agent::{AgentConfigData, MemoryEntry, Record, SingleSessionMD};
use fae_engine::AgentsEngine;
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut engine = AgentsEngine::default().await;
    let ws = engine.build_workspace("main", |_| {}).await;

    if ws.get_agent("main").await.is_err() {
        let config = AgentConfigData::default().set_prompt_path("../../docs/prompt/aicoding.txt");
        ws.create_single_agent("main", config.into_agent_config()).await?;
    }

    let mut session = ws
        .session_call_stream::<_, Record, Record>("main", SingleSessionMD::default())
        .await?;

    let stream = session
        .call_stream(Record::from_user_input("用一句话介绍 FAE"))
        .await?;

    tokio::pin!(stream);
    while let Some(record) = stream.next().await {
        print!("{}", record.content());
    }

    ws.exit().await;
    Ok(())
}
```

## 开发与验证

常用命令：

```bash
cargo fmt --all
cargo check --workspace
cargo check -p fae-agent
cargo check -p fae-engine
cargo check -p fae
cargo test --workspace
```

如果只是验证 CLI 参数解析，可以运行：

```bash
cargo run -p fae -- --help
cargo run -p fae -- agent --help
```

## License

Apache-2.0
