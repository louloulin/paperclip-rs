//! M3 anchor scaffold（LUM-1406）：runtimes / runtime-profile 台账切片 —— **空 router 占位**。
//!
//! 由 M3-4（`feat/multica-rs-m3b-runtime-profiles`）填充真实 handler，覆盖 docs/15 §1.1 的
//! 6 条 runtime-profile 路由 + §1.2 的 9 条 runtimes 台账路由；§1.2 剩下的 8 条异步往返
//! （`update` / `models` / `local-skills`）归 M3-7。本文件已由
//! `mount.rs::mount_slice_runtime()` 接好，切片只需在此实现 `router()`。
//!
//! 切片合并时要做的两件事（不在本片范围）：
//! 1. **删除** `mount.rs` 里 M0 的 `/api/runtimes` 占位（`get().post()` 整块；
//!    上游只有 `GET /api/runtimes/`，`POST` 无对应物，docs/15 §9.6.6）；
//! 2. 删除占位会让 `route_parity.py` 报 2 条 regression —— 切片**不跑**
//!    `--write-baseline`，由 M3 集成 cycle 统一刷新（docs/15 §7.3）。
//!
//! 注意（M1-D 实测踩过的坑）：axum 0.7（matchit 0.7）路径参数必须写 `:id`，不写 `{id}`。
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等对应 M3 切片填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
