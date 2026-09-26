//! `GET /health/realtime` —— realtime/daemonws 进程级计数器快照（**M10-3 原地填充**，
//! `docs/64` §2.3 / §4.1 第 4 行）。
//!
//! 上游：`server/cmd/server/health_realtime.go`（106 行）+ `internal/realtime/metrics.go`
//! 的 `Snapshot()`（13 个顶层键）+ `internal/daemonws/metrics.go`（14 个键，挂在
//! `snapshot["daemonws"]` 之下）。
//!
//! 四态访问门（`docs/64` §2.3 的表，逐条都要有用例）：
//! 已设 `REALTIME_METRICS_TOKEN` + 正确 `Bearer` ⇒ **200**；token 缺失/错误 ⇒ **401** +
//! `WWW-Authenticate: Bearer realm="metrics"`；未设 token + loopback 且无转发头 ⇒ **200**；
//! 未设 token + 非 loopback **或任一** `X-Forwarded-*`/`Forwarded` 存在 ⇒ **404**。
//!
//! ## anchor 期（本文件由 M10-0 `LUM-2102` 建桩）
//!
//! **空** `Router::new()` ⇒ **零注册键**。🔴 **不得**注册 501 占位（理由同 `live.rs`）。
//!
//! 形态：上游 plain `r.Get("/health/realtime", ...)` ⇒ 只注册**无尾斜杠**那一形态。
//!
//! ⚠️ 计数器一律落 `crates/mc-ws/src/hub/metrics.rs`（新文件），`hub/mod.rs` 只加字段与自增点
//! —— 否则会逼近门 ⑩ 的 800 行上限（`docs/64` §6.3）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/health/realtime` 切片（M10-3 在这里把访问门 + 快照 handler 填进来）。
pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
}
