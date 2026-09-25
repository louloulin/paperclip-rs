//! `GET /api/issues/:id/pull-requests`（`router.go:2011`）—— 写者 **M8-4**。
//!
//! # anchor 期的**唯一**注册键：501 占位（**原地搬运**，`docs/61` §3.1 / §9.7）
//!
//! 本路由原先注册在 `crate::routes::issues::router()`（`issues/mod.rs:193`，handler `not_implemented`）。
//! M8-0 把它**搬到这里**，因为它的真实现归 M8-4（GitHub PR 读面），而 M8-4 不该回来改
//! `issues/mod.rs`（那是冻结的 anchor 文件）。
//!
//! 搬运的三条不变量（`docs/61` §6.1 的 M8-0 行）：
//! 1. **注册键逐字不变**（`GET /api/issues/:id/pull-requests`）⇒ ⑦ 的 `local` 不变；
//! 2. **handler 名仍是 `not_implemented`**（`route_parity.py` 的占位正则只认
//!    `\bplaceholder\b` 与这套命名）⇒ `implemented_placeholder` 计数不变；
//! 3. 路由**必须存在**（M8-4 是把它换成真实现，**不能删**，`docs/61` §2.7 第 7 条）。
//!
//! # 真实现（M8-4）
//!
//! 读 `github_pull_request` + `issue_pull_request`，按 `issue_id` 收窄（越权返回 404），
//! 响应形状用 [`super::dto::GithubPullRequestResponse`]。

use axum::routing::get;
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片：anchor 期只有**搬运**过来的 501 占位。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/api/issues/:id/pull-requests",
        get(crate::routes::issues::not_implemented),
    )
}
