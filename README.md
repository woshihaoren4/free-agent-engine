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

CLI 会自动连接当前 FAE home 的后台服务，默认监听 `127.0.0.1:6666`。
关闭界面后，服务和未取消的任务继续运行。使用 `fae server status` 查看状态，`fae server show` 查看 PID、地址、端口等服务信息，
`fae server stop` 停止并等待清理；自定义端口可用 `fae server start --port 2666`。
端口冲突会直接报错。更多服务、会话和重连说明见 [CLI 文档](app/fae/README.md)。

支持 Python Agent、工具和流式模型插件。先安装 SDK，使用
`fae plugin install <文件或目录>` 安装，`fae plugin show <id>` 查看详情，
`fae plugin show`（等同于 `fae plugin show '*'`）简洁列出插件，
`fae plugin uninstall <id>` 卸载。
安装记录保存在 `<fae_home>/plugins/plugins.config`；服务启动按安装顺序加载
已安装且启用的插件当前源码，任何加载错误都会导致启动失败。服务运行时安装、覆盖和卸载即时生效，
卸载保留源码并更新配置；服务未运行时仅更新文件及配置，下次启动按配置加载。
支持 Rust 逐插件调用的 direct 模式和 Python 洋葱链 chain 模式，
配置、完整 API 和可运行样例见
[Python 插件 SDK](python/fae-plugin-py/README.md)。

会话使用公共 v4 协议，Python 插件 API 为 `0.2.0`，升级时需同步更新服务、
客户端和 SDK。逻辑 Session 以可信用户和 session ID 标识，每次 start 创建独立
Execution；同一会话最多一个活跃执行，steer 可查询 Applied/NotApplied，
stop 检查 execution ID 并等待子调用和插件资源清理。多个客户端可以订阅同一
会话，断线保留执行。Agent/Workflow 定义与 workspace 在每次 Execution 固定，
下一次执行重新加载定义；历史和用户记忆沿用已有分区、文件路径及格式。

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
use fae_agent::{protocol::ContentBlock, *};
use fae_engine::{Engine, SessionCoordinator};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = Engine::default().await;
    let target = PlanTarget::Agent { agent_id: "fae".into() };
    let workspace = std::env::current_dir()?.to_string_lossy().into_owned();
    let session = SessionCoordinator::new(engine.clone(), ContextData {
        user_info: UserInfo { user_id: UserId::new("master")?, name: None },
        session_config: SessionConfig {
            session_id: SessionId::new("demo")?,
            name: "Demo".into(),
            lifecycle: SessionLifecycle::Open,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            target: target.clone(),
            workspace: WorkspaceBinding { root: workspace.clone(), project_files: vec![] },
            options: ExecutionOptions::default(),
            channels: vec![],
        },
        memory_view: MemoryView::default(),
        latest_execution_view: None,
    })?;
    let execution = session.prepare(ExecutionSettings {
        target, workspace, options: ExecutionOptions::default(), definitions: vec![],
    })?;
    let execution_id = execution.execution_id.clone();
    let (agent, _) = SingleAgentEnv::from_agent_id("fae", "用一句话介绍 FAE");
    engine.execute_session(execution, agent.with_session_id("demo")).await?;
    let outcome = session.execution(Some(&execution_id))?.outcome
        .ok_or_else(|| anyhow::anyhow!("Execution is still cleaning up"))?;
    match outcome {
        ExecutionOutcome::Succeeded { result } => {
            for block in result {
                if let ContentBlock::Text { text } = block { println!("{text}"); }
            }
        }
        ExecutionOutcome::Failed { error } => eprintln!("{}", error.message),
        ExecutionOutcome::Cancelled { reason } => eprintln!("{reason}"),
    }
    // 结果可重复查询；下一次 prepare 会创建新的 Execution。
    assert!(session.execution(Some(&execution_id))?.outcome.is_some());
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
- [x] hook规范化
- [x] 多session通信改造
- [x] 消息规范化
