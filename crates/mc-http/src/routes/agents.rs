//! M3 anchor scaffold（LUM-1406）：agent 面 `/api/agents*` 切片 —— **空 router 占位**。
//!
//! 由 M3-5（`feat/multica-rs-m3b-agents`）填充真实 handler，覆盖 docs/15 §1.3 的 16 条
//! （13 条 `/api/agents` + 3 条 workspace 级统计，含 `labels` 3 条 / `env` 2 条 /
//! `cancel-tasks`）。本文件已由 `mount.rs::mount_slice_agent()` 接好，
//! 切片只需在此实现 `router()`，**无需改动** `mount.rs` / `mod.rs`。
//!
//! 切片合并时要做的两件事（不在本片范围）：
//! 1. **删除** `mount.rs` 里 M0 的 `/api/agents` 占位（`get().post()` 整块，
//!    docs/15 §9.6.6：只删一个方法会留一条恒 501 幽灵路由）；
//! 2. 删除占位会让 `route_parity.py` 报 2 条 regression —— 切片**不跑**
//!    `--write-baseline`，由 M3 集成 cycle 统一刷新（docs/15 §7.3）。
//!
//! 注意（M1-D 实测踩过的坑）：axum 0.7（matchit 0.7）路径参数必须写 `:id`，
//! `{id}` 会被当字面量段——编译通过但恒 404（docs/15 §7.3）。
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等对应 M3 切片填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
