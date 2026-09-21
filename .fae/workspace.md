# Free Agent Engine (FAE / 风筝) 项目说明

## 项目概述
FAE 是一个用 Rust 编写的 Agent 开发框架，提供统一抽象帮助开发者快速构建和管理智能体运行系统。

## 核心特性
- **计划与执行分离**：规划逻辑和执行逻辑解耦，支持定义复杂计划
- **多层次抽象**：提供 Agent、Tool、Skill 等抽象，同时支持底层定制
- **ReAct 架构**：基于 ReAct 范式，支持会话、心跳等内在行为
- **任务并行分发**：支持多计划并行执行
- **Workflow 支持**：内置工作流能力
- **多 Session 通信**：支持多会话间通信

## 项目结构
```
free-agent-engine/
├── app/fae/           # FAE CLI 入口（内置实战案例）
├── crates/
│   ├── fae-agent/     # Agent 核心抽象与实现
│   ├── fae-engine/    # 引擎运行时
│   ├── async-openai/  # OpenAI 异步客户端封装
│   └── async-openai-macros/
├── examples/          # 示例代码
├── fae-planner/       # 规划器模块
├── scripts/           # 构建/安装脚本
└── docs/              # 文档
```

## Workspace 成员
- `app/fae` - CLI 应用
- `crates/fae-agent` - Agent 核心库
- `crates/fae-engine` - 引擎库
- `crates/async-openai` - OpenAI 客户端
- `examples` - 示例项目

## 注意事项
- 项目使用 Rust 2024 edition，需要较新版本的 Rust 工具链
- target 目录为构建产物，不需要关注，且任何命令都应该避免操作这个路径
