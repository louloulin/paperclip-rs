//! M3 anchor scaffold（LUM-1406）：daemon 面 `/api/daemon*` + ws 服务端切片 —— **空 router 占位**。
//!
//! 由 M3-7（`feat/multica-rs-m3c-daemon`）填充真实 handler，覆盖 docs/15 §1.5 的 36 条 +
//! §1.2 的 8 条异步往返（`Initiate*` → `…/result` → `Get*`）。本文件已由
//! `mount.rs::mount_slice_daemon()` 接好，切片只需在此实现 `router()`。
//!
//! 体量警告（docs/15 §6 M3-7 / §7.7 的 R7）：上游 `internal/daemon/daemon.go` 6056 行、
//! `internal/daemonws/*` 1370 行 ⇒ 本文件必然超 800 行上限，
//! 预先按域拆成 `routes/daemon/{register,heartbeat,claims,tasks,requests,gc}.rs`，
//! 每个 ≤800 行，本文件只做 `merge` 聚合。
//!
//! 协议类型走 `mc-daemon-proto`（M3-1 冻结）；ws 服务端在 `crates/mc-ws` + `mc-realtime`
//! （M3-7 是唯一写这两者的切片）；client 在 `crates/mc-daemon`。
//!
//! 注意（M1-D 实测踩过的坑）：axum 0.7（matchit 0.7）路径参数必须写 `:id`，不写 `{id}`。
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等对应 M3 切片填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
