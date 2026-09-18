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
（`x86_64`）。安装器会校验下载文件的 SHA-256，并将 `fae` 安装到系统
`PATH` 目录 `/usr/local/bin`。目录不可写时，安装器会自动通过 `sudo`
请求管理员权限，无需额外配置 `PATH`。

按照 [fae CLI 配置说明](app/fae/README.md) 初始化默认 Agent，设置模型 API
Key 后启动：

```bash
export OPENAI_API_KEY="sk-..."
fae init
fae
```

## 快速开始

完成上面的 `fae init` 后，新建 Rust 项目并添加依赖：

```bash
cargo new fae-demo
cd fae-demo
cargo add fae-agent fae-engine \
  --git https://github.com/woshihaoren4/free-agent-engine
cargo add anyhow
cargo add tokio --features macros,rt-multi-thread
```

将 `src/main.rs` 替换为：

```rust
use fae_agent::{Session, SessionEventData, SingleAgentEnv};
use fae_engine::Engine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = Engine::default().await;
    let (agent, session) = SingleAgentEnv::from_agent_id("fae", "用一句话介绍 FAE");
    let task = engine.launch(agent).await?;

    while let Some(event) = session.answer().await? {
        match event.event_data()? {
            SessionEventData::Completed { content } => {
                println!("{content}");
                break;
            }
            SessionEventData::Failed { error } => anyhow::bail!("{error}"),
            _ => {}
        }
    }

    task.result::<()>().await?;
    engine.exit().await?;
    Ok(())
}
```

运行：

```bash
export OPENAI_API_KEY="sk-..."
cargo run
```

## TODO

- [x] agent 任务的并行分发
- [x] 多计划执行
- [x] workflow
- [ ] hook规范化
- [x] 多session通信改造
- [x] 消息规范化
