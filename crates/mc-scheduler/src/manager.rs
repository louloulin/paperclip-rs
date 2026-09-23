//! 主循环：每 tick 取计划、认领、执行、心跳。
//!
//! - **写者**：M5-7。
//! - **上游**：`scheduler/manager.go`489（490）—— `plansForTick`95 + `runClaimed`116 +
//!   `runHeartbeats`33 + `classifyError`23（分类在 `error.rs`）。
//! - **形态**：`tokio::time::interval` + `tokio::spawn`，取消用 `CancellationToken`
//!   （`tokio-util` 已在本 crate 依赖表里）。
//! - **空注册表也能跑**：M5-7 交付时注册表为空 ⇒ 本循环必须能空转并干净退出（可独立验收）。
//! - **单进程多实例安全**：任何「只跑一次」的判定都要过 `db_ops.rs` 的租约，不要用进程内静态量。
