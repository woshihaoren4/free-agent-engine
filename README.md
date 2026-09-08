# Free Agent Engine

FAE，中文风筝。 是一个用 Rust 编写的agent开发框架。它提供一组统一的抽象，帮助开发者快速构建和管理智能体运行系统。其特点如下：
- 计划和执行分离：设计上将智能体的规划和执行逻辑分开，开发者可以定义任意复杂的计划，而不需要关心具体的执行细节。
- 多层次实现：fae会提供一系列具体的抽象，方便开发者快速接入agent，tool，skill等，也允许实现一个更底层的抽象来任意定制自己的需求。
- ReAct架构：fae整体采用ReAct范式，支持agent的计划和执行，也完善了agent的内在行为，如会话，心跳等。


## 快速体验

fae引擎内置了一个实战案例，即fae的cli，你可以快速安装并体验它：

```bash
curl --proto '=https' --tlsv1.2 -sSfL https://woshihaoren4.github.io/free-agent-engine/bin/install.sh | bash
```

当前发布包支持 Apple Silicon macOS（`arm64`）和 64 位 Linux
（`x86_64`）。安装器会校验下载文件的 SHA-256，并默认安装到 `PATH`
中的可写目录或 `~/.local/bin`。也可以通过 `INSTALL_DIR` 指定安装目录：

```bash
curl --proto '=https' --tlsv1.2 -sSfL https://woshihaoren4.github.io/free-agent-engine/bin/install.sh \
  | INSTALL_DIR="$HOME/bin" bash
```

按照 [fae CLI 配置说明](app/fae/README.md) 初始化默认 Agent，设置模型 API
Key 后启动：

```bash
export OPENAI_API_KEY="sk-..."
fae init
fae
```

### 发布 CLI

在 macOS 上安装 Rust、[Zig](https://ziglang.org/) 和
[`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild)，然后执行：

```bash
./scripts/build-fae-ctl.sh
```

脚本使用 `Cargo.lock` 编译 release 版本，并更新以下 GitHub Pages 文件：

```text
docs/bin/mac/fae
docs/bin/mac/fae.sha256
docs/bin/linux/fae
docs/bin/linux/fae.sha256
```

提交这些产物以及 `docs/bin/install.sh` 并推送到 GitHub。仓库的 GitHub
Pages Source 需要设置为当前发布分支的 `/docs` 目录；Pages 部署完成后，
上面的安装命令即可使用。

## 快速开始

最小用法：

先创建一个prompt放在`prompt.txt`文件中，例如：
```
你是一个专业的代码助手，你的任务是根据用户的问题，生成符合要求的代码。
```
然后在`main.rs`中使用它：

```rust
use fae_agent::{AgentConfigData, MemoryEntry, Record, SingleSessionMD};
use fae_engine::AgentsEngine;
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut engine = AgentsEngine::default().await;
    let ws = engine.build_workspace("main", |_| {}).await;

    if ws.get_agent("main").await.is_err() {
        let config = AgentConfigData::default().set_prompt_path("prompt.txt");
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

## TODO

- [x] agent 任务的并行分发
- [x] 多计划执行
- [x] workflow
- [ ] hook规范化
- [x] 多session通信改造
- [ ] 消息规范化
