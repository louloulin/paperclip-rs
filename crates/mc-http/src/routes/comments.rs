//! M2 anchor scaffold（LUM-1347 / M1-D 预扩展）：评论 CRUD / resolve / 评论级 reactions（见 docs/12-M2-COMMENT.md）。
//!
//! 由 M2-B（LUM-1349） 填充真实 handler；本文件已由 `mount.rs::mount_slice_*()` 接好，
//! 切片只需在此实现 `router()`，无需改动 `mount.rs` / `mod.rs`。
//!
//! 注意（M1-D 实测踩过的坑）：axum 0.7（matchit 0.7）路径参数必须写 `:id`，
//! `{id}` 会被当字面量段——编译通过但恒 404。
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等对应 M2 切片填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
