//! M3-7（LUM-1438）：daemon 面 `/api/daemon*` + ws 服务端。
//!
//! 按域拆文件，每个 ≤800 行（⑩ 门）：`claims`（claim 批）/ `tasks`（任务生命周期）/
//! `lifecycle`（注册、心跳、下线、ws 升级）/ `scope`（daemon 鉴权提取器）/
//! `skills`（skill bundle 打包）/ `dto`（请求响应类型）。
//!
//! 本文件只做聚合与 `router()`；`mount.rs::mount_slice_daemon()` 已接好本模块。
//!
//! 注意（M1-D 实测踩过的坑）：axum 0.7（matchit 0.7）路径参数必须写 `:id`，不写 `{id}`。
pub mod claims;
pub mod dto;
pub mod lifecycle;
pub mod scope;
pub mod skills;
pub mod tasks;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// daemon 面路由表。接线进度见 LUM-1438 描述的「表 A」。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
