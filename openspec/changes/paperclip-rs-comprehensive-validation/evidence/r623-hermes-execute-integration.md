# R623 — Hermes Gateway execute.rs 整合 + 编译闭环

## 背景

R622 写入了 `pc-adapter-hermes-gateway` 的 `sse_client` / `dashboard` / `retry_policy`
模块并新增了 19 个 lib + 7 个 e2e 测试。R622 末期新增的 `execute.rs`（989 行）
**未在 `lib.rs` 公开**，且仍引用占位 helper `crate::lib_helper_build_session_key`，
整个 crate 因此无法编译。本轮补齐三处 R622 残留的集成问题，让
`HermesGatewayAdapterV2` 真正成为默认生产入口。

## 改动

- `crates/pc-adapter-hermes-gateway/src/lib.rs`
  - `pub mod execute;`
  - `HermesGatewayAdapter::execute` 委托给 `execute::HermesGatewayAdapterV2::new()`
  - 移除旧版 CLI spawn 逻辑
  - 删除未使用的 `pc_adapter_process` 导入
- `crates/pc-adapter-hermes-gateway/src/execute.rs`
  - 修掉 `crate::lib_helper_build_session_key` → 复用 `super::build_session_key`
  - `DefaultHermesExecuteClient::new` 把 `api_key` 显式消费一次（避免 `Into<String>` move 错误）
  - `FakeHermesExecuteClient::poll_until_terminal` 现在容忍脚本里夹杂 `Event` 步骤
  - 给 `execute_with_client` 加上 SSE 消费任务的失败回压：
    - poll 失败时立即 `abort()` SSE 任务
    - 终态到达后给消费任务设 1–8s 的 graceful drain 上限
  - 在 `transport_security` 之前强制只接受 `http` / `https` scheme，
    错误转化为 `AdapterError::InvalidConfiguration`
- `crates/pc-adapter-hermes-gateway/Cargo.toml`
  - `chrono = { workspace = true }`
  - `uuid = { workspace = true }` 提到 runtime 依赖
  - dev-dep 仍然保留 `uuid` 供测试用

## 验证

- `cargo check -p pc-adapter-hermes-gateway --tests` 0 error
- `cargo test -p pc-adapter-hermes-gateway --lib` → **68 passed**
- `cargo test -p pc-adapter-hermes-gateway --test sse_e2e` → **7 passed**
- `cargo test -p pc-adapter-cursor-cloud -p pc-adapter-openclaw-gateway -p pc-adapter-hermes --lib` → **1190 passed**

## 后续

- R624：把 `pc-server` 的 `CursorCloudAdapter::new()` / `OpenclawGatewayAdapterV2::new()`
  切到真实 transport factories（见 `r624-production-transport-switch.md`）。
- 真正生产 sign-and-connect 的 Ed25519 仍属 R624.1 范围。
