//! `GET /health` —— liveness 探针（**M10-1 原地填充**，`docs/64` §2.1 / §4.1 第 2 行）。
//!
//! 上游：`server/cmd/server/health.go::liveHandler`（`f41fae6b08fb` L85-91）——
//! **进程活着就 200，不触库**；body `{status:"ok", pid, commit, started_at}`（后三者 omitempty）。
//!
//! ## anchor 期（本文件由 M10-0 `LUM-2102` 建桩）
//!
//! **空** `Router::new()` ⇒ **零注册键**。🔴 **不得**在这里注册 501 占位：
//! `/health` 在 ⑨ 有 1 条 fixture（`health/001-TestHealth-L212`，`actor=anonymous`），
//! 占位会把它从 `unmounted` 变成 **`mismatch`**（期望 200 / 得到 501）。
//!
//! 形态：上游是 plain `r.Get("/health", health.liveHandler)` ⇒ 只注册**无尾斜杠**那一形态
//! （`docs/64` §1.4 实测 `dual-form required: 0`）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/health` 切片（M10-1 在这里把 handler 与 `.route("/health", get(handler))` 填进来）。
pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
}
