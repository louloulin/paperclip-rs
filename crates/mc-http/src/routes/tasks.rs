//! M3 anchor scaffold（LUM-1406）：task / agent-builder 用户面切片 —— **空 router 占位**。
//!
//! 由 M3-6（`feat/multica-rs-m3b-task-queue`）填充真实 handler，覆盖 docs/15 §1.4 的 4 条
//! agent-builder 路由 + §1.6 的 11 条 task / lifecycle / usage / retry 路由。本文件已由
//! `mount.rs::mount_slice_task()` 接好，切片只需在此实现 `router()`。
//!
//! ⚠️ 切片必读（docs/15 §9.6.2 / §7.1）：那 11 条里有 **6 条已经以 `not_implemented`
//! stub 形式注册在 `routes/issues.rs`**（preview-trigger / active-task / rerun /
//! task-runs / `{id}/tasks/{taskId}/cancel` / `{id}/usage`，L91/123/124/125/126/133）。
//! axum 0.7 对**同 path + 同 method** 重复注册会直接 panic ⇒ 必须**原地替换 handler**
//! （或把路由移入本切片后删除原注册），**不能**在本文件里新增同名路由。
//!
//! 注意（M1-D 实测踩过的坑）：axum 0.7（matchit 0.7）路径参数必须写 `:id`，不写 `{id}`。
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等对应 M3 切片填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
