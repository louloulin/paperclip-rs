//! Router 切片聚合：各领域模块独立注册点。
//!
//! 设计目的：
//! - 每个 M1 / M2 / ... sub-issue 添加新文件 `auth.rs` / `workspaces.rs` / ...
//! - 每个 sub-issue 只在自己新增的 mount_slice 函数内追加 `.merge(...)`
//! - 多分支并发开发时只读不写公共 anchor，避免 3-way merge 冲突
//!
//! 公共 anchor（本文件）：仅维护一个稳定的 `Router::new()` + 健康/占位切片。

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

use super::health;
use super::openapi;
use crate::state::AppState;

// M1 sub-issue C 的模块（invitations / pats / auth_user）在 routes/mod.rs 中声明。
// 本文件仅负责把各 sub-issue 的 router 切片合并到全局 router。

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // ----- 健康 / OpenAPI / 通用 -----
        .route("/api/health", get(health::health))
        .route("/api/health/db", get(health::db_health))
        .route("/api/openapi.json", get(openapi::openapi_json))
        // ----- M0 占位（M1+ 各 sub-issue 用真实 handler 替换） -----
        .route("/api/auth/login", post(health::placeholder))
        .route("/api/auth/logout", post(health::placeholder))
        .route("/api/auth/session", get(health::placeholder))
        .route(
            "/api/workspaces",
            get(health::placeholder).post(health::placeholder),
        )
        .route("/api/workspaces/{id}", get(health::placeholder))
        .route("/api/workspaces/{id}/members", get(health::placeholder))
        .route("/api/issues", get(health::placeholder).post(health::placeholder))
        .route("/api/issues/{id}", get(health::placeholder))
        .route("/api/agents", get(health::placeholder).post(health::placeholder))
        .route("/api/runtimes", get(health::placeholder).post(health::placeholder))
        .route(
            "/api/chat/sessions",
            get(health::placeholder).post(health::placeholder),
        )
        .route("/api/inbox", get(health::placeholder))
        .route("/api/skills", get(health::placeholder).post(health::placeholder))
        .route("/api/plugins", get(health::placeholder).post(health::placeholder))
        .route(
            "/api/autopilots",
            get(health::placeholder).post(health::placeholder),
        )
        .route("/api/squads", get(health::placeholder).post(health::placeholder))
        .route("/api/projects", get(health::placeholder).post(health::placeholder))
        .route("/api/comments", get(health::placeholder).post(health::placeholder))
        .route("/api/feature-flags", get(health::placeholder))
        // ----- M1 切片占位（sub-issue A/B/C 在 mount_slice_* 里追加真实 router） -----
        .merge(mount_slice_workspace_member())
        .merge(mount_slice_auth())
        .merge(mount_slice_invitation())
        .merge(mount_slice_pat())
}

/// workspace + member + me 切片。
///
/// 由 M1 sub-issue A 填充：crates/mc-http/src/routes/workspaces.rs 真实 handler 后
/// 在本函数里 `.merge(workspaces::router())`。
fn mount_slice_workspace_member() -> Router<Arc<AppState>> {
    Router::new()
}

/// auth 切片：send-code / verify-code / logout / refresh / me。
///
/// 由 M1 sub-issue B 填充：crates/mc-http/src/routes/auth.rs 真实 handler 后
/// 在本函数里 `.merge(auth::router())`。
fn mount_slice_auth() -> Router<Arc<AppState>> {
    Router::new()
}

/// invitation 切片：workspace invitation + 我的 invitation + accept/decline。
///
/// 由 M1 sub-issue C 填充：crates/mc-http/src/routes/invitations.rs 真实 handler 后
/// 在本函数里 `.merge(invitations::router())`。
///
/// 子 router 仅声明路由表，不在内部 `with_state` —— 真正的 state 由
/// `apps/mc-server/src/main.rs` 在调用 `mc_http::routes::router().with_state(state)` 时
/// 一次性注入。
fn mount_slice_invitation() -> Router<Arc<AppState>> {
    super::invitations::router()
}

/// PAT 切片：list / create / revoke PAT。
///
/// 由 M1 sub-issue C 填充：crates/mc-http/src/routes/pats.rs 真实 handler 后
/// 在本函数里 `.merge(pats::router())`。
fn mount_slice_pat() -> Router<Arc<AppState>> {
    super::pats::router()
}
