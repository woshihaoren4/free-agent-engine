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
设置模型和参数

```bash
export OPENAI_API_KEY="sk-..."
export FAE_DEFAULT_MODEL="gpt-xxx"
```

然后初始化并启动：

```bash
fae init
fae agent --chat
```


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
