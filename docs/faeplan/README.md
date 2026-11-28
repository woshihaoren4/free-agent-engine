# FAE 实现文档

这组文档以仓库当前实现为准。使用和安装命令见 [FAE CLI](../../app/fae/README.md)，Python 接口和示例见 [插件 SDK](../../python/fae-plugin-py/README.md)。

| 文档 | 内容 |
| --- | --- |
| [服务、会话与协议](service-and-session.md) | 后台进程、会话身份、执行与清理、WebSocket 协议、存储与重连 |
| [Plan、Runtime 与 Hook 插件](plugins.md) | 插件声明与组合、Rust/JSON 边界、Worker 生命周期和流 |
| [Agent Prompt 与模型请求](prompt.md) | System 各段来源、历史窗口、工具声明、压缩与工作区 |
| [飞书网关](lark.md) | 配置、消息和话题映射、控制命令、结果投递与恢复 |
| [定时器](timer.md) | Cron、工具契约、持久化与触发后的新会话 |
