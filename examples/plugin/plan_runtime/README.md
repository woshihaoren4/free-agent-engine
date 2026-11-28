# Plan / Runtime 示例

在仓库根目录安装 SDK 和插件，启动服务：

```bash
python3 -m pip install -e ./python/fae-plugin-py
cargo build -p fae
./target/debug/fae plugin install examples/plugin/plan_runtime
./target/debug/fae server start
```

通过服务的控制 WebSocket 发送以下 JSON（连接需要该 FAE home 的服务 token；
控制连接和认证方式与 CLI 相同）。`audit_plan` 将调用 `report_plan`，
后者提交任务给 EngineRuntime 中注册的 `report_runtime`。

```json
{
  "api_version": "0.1.0",
  "request_id": "report-1",
  "event": "/request/plan/call",
  "session": null,
  "plan": {"plan_id": "audit_plan"},
  "payload": {"source": "weekly"}
}
```

成功响应为：

```json
{
  "api_version": "0.1.0",
  "response_id": "report-1",
  "session": null,
  "plan": {"plan_id": "audit_plan"},
  "code": 0,
  "msg": "ok",
  "payload": {"source": "weekly"}
}
```

直接调用 Runtime 时，使用 `event: "/request/runtime/exec"`、
`plan: null` 与 `call: {"runtime_id": "report_runtime"}`，payload 保持原值。
`/request/runtime/select` 同样可调用此示例。

流示例将 payload 改为 `{"stream":true,"items":["first","second"]}`。
Plan 通过 `complete(..., streams=[id])` 转交流租约，release 后仍可消费。
响应 payload 为 `{"kind":"streaming","stream_id":"宿主签发的ID"}`。
接收方调用 `/request/stream/next`，payload 为
`{"owner_request_id":"report-1","stream_id":"宿主签发的ID"}`，
session/plan 均为 null；每次请求必须使用新的 request_id。
返回 `{"done":false,"value":"first"}`，最后返回 `{"done":true}`。
提前停止时使用 `/request/stream/close` 和相同 payload。
引用最长保留 120 秒，并绑定原 Engine 代际和 Context；不能用于其他执行。
