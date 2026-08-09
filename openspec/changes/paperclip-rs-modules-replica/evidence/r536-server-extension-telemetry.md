# R536 / M36 — Extension 注入端到端遥测

## 本轮完成

在不修改 `AppState` 的前提下，新增 `pc-telemetry/tests/server_extension_e2e.rs`，演示：

1. `ProductTelemetryClient` 注入 `axum::Extension`。
2. 路由处理器内调用 `track("issue.created", dimensions)`。
3. 真 HTTP POST 命中本地 collector，校验 envelope 与 `dimensions` 完整。
4. 失败重试 + actor 调度对调用方透明。

## 验证

- `cargo test -p pc-telemetry --all-targets`：29/29（含新增 1 个）。
- `cargo check -p pc-server`：0 errors。

## 影响

此模式说明后续 route 接入 `track()` 不必改 `AppState`，可通过 `Router::layer(Extension(client))` 局部注入；为业务事件埋点接入奠定无侵入基础。
