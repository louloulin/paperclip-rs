# R624 — 三类 adapter 生产路径切到真实 transport

## 背景

R616 / R617 已经实现 `TungsteniteWireClient` (WS) 与 `ReqwestCursorCloudClient` (HTTP)，
但 `pc-server` 启动时仍以 `Default` (Fake) 注入 Cursor Cloud、OpenClaw、Hermes Gateway。
本轮把 server 入口改造成「按环境变量决定真/假 client」，让生产部署可以零代码改动切到
真实 transport，同时保留本地/CI 回退路径。

## 改动

- `crates/pc-adapter-cursor-cloud/src/lib.rs`
  - `pub use crate::execute::CursorCloudAdapter;`
- `crates/pc-adapter-cursor-cloud/src/execute.rs`
  - `CursorCloudAdapter::for_runtime(base_url, api_key)` 工厂：
    - `api_key` 非空时构造 `ReqwestCursorCloudClient`
    - 空 base_url 时使用 `https://api.cursor.com` 默认值
    - `api_key` 空时回退到 `FakeCursorCloudClient`
- `crates/pc-adapter-openclaw-gateway/src/execute.rs`
  - `OpenclawGatewayAdapterV2` 持有 `DynWireClient`，新增 `with_client` / `for_runtime`
  - `for_runtime(base_url, identity)` 注入 `FakeWireClient::for_runtime_url`，
    把运行时上下文暂存到 fake 上（等待 R624.1 替换为 `TungsteniteWireClient::connect`）
- `crates/pc-adapter-openclaw-gateway/src/lib.rs`
  - `pub use crate::execute::OpenclawGatewayAdapterV2;`
- `crates/pc-adapter-openclaw-gateway/src/wire_client.rs`
  - `FakeWireClient` 新增 `runtime_url` / `runtime_identity` 字段
  - 新增 `for_runtime_url`、`runtime_url()`、`runtime_identity()` 三个 API
- `apps/pc-server/src/main.rs`
  - 引入 `OpenclawGatewayAdapterV2`
  - 用 `build_cursor_cloud_adapter()` / `build_openclaw_gateway_adapter()` 替换
    `CursorCloudAdapter::new()` / `OpenclawGatewayAdapter::new()`
  - 工厂读取环境变量：
    - `CURSOR_CLOUD_BASE_URL` + `CURSOR_API_KEY`
    - `OPENCLAW_GATEWAY_URL` + `OPENCLAW_GATEWAY_IDENTITY_PEM` + `OPENCLAW_GATEWAY_DEVICE_ID`
- `crates/pc-adapter-grok-local/Cargo.toml`
  - `tokio = { workspace = true }` 移到 runtime 依赖（skills.rs 在生产代码用到
    `tokio::fs`，不是只测试用）

## 验证

- `cargo check -p pc-server --bins --tests` → **0 errors**
- `cargo test -p pc-adapter-cursor-cloud -p pc-adapter-openclaw-gateway -p pc-adapter-hermes-gateway --lib` → **368 passed**
- `cargo test -p pc-adapter-cursor-cloud -p pc-adapter-openclaw-gateway -p pc-adapter-hermes-gateway --tests` → **391 passed (9 suites)**
- 其它 5 个 adapter `cargo test ... --lib` → **1190 passed**

## 仍需完成

- R624.1：完成 OpenClaw `sign-and-connect` 真正 Ed25519 实现，把
  `FakeWireClient::for_runtime_url` 替换为 `TungsteniteWireClient::connect`。
- R624.2：把 `pc-server` 的 OpenClaw / Cursor Cloud 工厂在启动时打印
  `tracing::info!` 标记真实 client 启用与否，便于运维确认。
- Hermes Gateway：本轮 `HermesGatewayAdapter::execute` 已经委托给 V2，
  V2 内部直接构造 `DefaultHermesExecuteClient`（reqwest + SSE），等 R625
  接入从 env 注入 session key 后即可让真实 Hermes Gateway 联通。
