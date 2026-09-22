//! Router 切片聚合：各领域模块独立注册点。
//!
//! 设计目的：
//! - 每个 M1 / M2 / ... sub-issue 添加新文件 `auth.rs` / `workspaces.rs` / ...
//! - 每个 sub-issue 只在自己新增的 `mount_slice` 函数内追加 `.merge(...)`
//! - 多分支并发开发时只读不写公共 anchor，避免 3-way merge 冲突
//!
//! 公共 anchor（本文件）：仅维护一个稳定的 `Router::new()` + 健康/占位切片。

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

use super::auth;
use super::health;
use super::openapi;
use super::workspaces;
use crate::state::AppState;

// M1 sub-issue C 的模块（invitations / pats / auth_user）在 routes/mod.rs 中声明。
// 本文件仅负责把各 sub-issue 的 router 切片合并到全局 router（签名以 A 的
// `router(state)` 为基，B/C 的无参 router() 在集成后统一收敛到这里）。

#[allow(clippy::needless_pass_by_value)] // M1-D 集成契约签名：main.rs 以 Arc 传入并最终 `with_state(state)`。
pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        // ----- 健康 / OpenAPI / 通用 -----
        .route("/api/health", get(health::health))
        .route("/api/health/db", get(health::db_health))
        .route("/api/openapi.json", get(openapi::openapi_json))
        // ----- M0 占位（M1+ 各 sub-issue 用真实 handler 替换） -----
        // `/api/auth/logout` 占位已删（M1-D / LUM-1347）：B 切片的真实 logout 是
        // 上游路径 `POST /auth/logout`（无 `/api` 前缀，见 docs/09 §8.4），两者并存会
        // 出现"看似可用其实是 empty 200 占位"的假路由。`/api/auth/login` 与
        // `/api/auth/session` 上游没有同路径路由（M0 自造），保留给 M2+ 实现。
        .route("/api/auth/login", post(health::placeholder))
        .route("/api/auth/session", get(health::placeholder))
        // 注：`/api/workspaces` 与其 `{id}` 占位路由已由 sub-issue A 的
        // mount_slice_workspace_member 真实路由替换（axum 0.7 同 path+method
        // 重复注册会 panic，不能共存）。
        // 同理，M0 的 `/api/issues`、`/api/issues/:id`、`/api/comments`、`/api/inbox`
        // 占位已由 M1-D（LUM-1347）删除：它们分别属于 M2-A（mount_slice_issue）与
        // M2-C（mount_slice_inbox），占位留着会在 M2 切片合并时撞成重复注册 panic。
        // 另外，M2 切片的真实 router 里路径参数必须写 `:id`（axum 0.7 / matchit 0.7
        // 会把 `{id}` 当字面量段——编译通过但恒 404，docs/09 §7.4）。
        .route(
            "/api/agents",
            get(health::placeholder).post(health::placeholder),
        )
        .route(
            "/api/runtimes",
            get(health::placeholder).post(health::placeholder),
        )
        .route(
            "/api/chat/sessions",
            get(health::placeholder).post(health::placeholder),
        )
        .route(
            "/api/skills",
            get(health::placeholder).post(health::placeholder),
        )
        .route(
            "/api/plugins",
            get(health::placeholder).post(health::placeholder),
        )
        .route(
            "/api/autopilots",
            get(health::placeholder).post(health::placeholder),
        )
        .route(
            "/api/squads",
            get(health::placeholder).post(health::placeholder),
        )
        .route(
            "/api/projects",
            get(health::placeholder).post(health::placeholder),
        )
        .route("/api/feature-flags", get(health::placeholder))
        // ----- M1 切片占位（sub-issue A/B/C 在 mount_slice_* 里追加真实 router） -----
        .merge(mount_slice_workspace_member(state.clone()))
        .merge(mount_slice_auth())
        .merge(mount_slice_invitation())
        .merge(mount_slice_pat())
        .merge(mount_slice_share_link())
        // ----- M2 切片占位（anchor scaffold 已接好，切片只需填自己的 router） -----
        .merge(mount_slice_issue())
        .merge(mount_slice_comment())
        .merge(mount_slice_inbox())
        .merge(mount_slice_subscriber())
}

/// workspace + member + me 切片。
///
/// M1 sub-issue A：真实 handler 在 crates/mc-http/src/routes/workspaces.rs，
/// 注入 `State(state: Arc<AppState>)` 后 merge 进来。
#[allow(clippy::needless_pass_by_value)] // 同 `router`：切片签名保持 Arc 传入，内部 clone 分发。
fn mount_slice_workspace_member(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new().merge(workspaces::router(state.clone()))
}

/// auth 切片：send-code / verify-code / logout / refresh / me。
///
/// 由 M1 sub-issue B 填充：crates/mc-http/src/routes/auth.rs 真实 handler 后
/// 在本函数里 `.merge(auth::router())`。
fn mount_slice_auth() -> Router<Arc<AppState>> {
    auth::router()
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

/// share-link 切片：workspace share link 的创建/列出/撤销 + 公开查看/加入。
///
/// M1-D（LUM-1347）从 LUM-1335（`feat/multica-rs-m1`）cherry-pick：真实 handler 在
/// crates/mc-http/src/routes/share_links.rs（独立文件，避开 A/C 的既有路由文件）。
fn mount_slice_share_link() -> Router<Arc<AppState>> {
    super::share_links::router()
}

// ---------------------------------------------------------------------------
// M2 anchor scaffold（M1-D / LUM-1347）
// ---------------------------------------------------------------------------
//
// 四个空切片先接好，三个 M2 分支各自只实作自己的 `routes/*.rs`，不再分别改本文件
// （与 M0 的 `056d2ae` 预扩展同一手法）。stub router 目前是空 `Router::new()`。

/// issue 切片：`/api/issues*` + `/api/issue-statuses*`（M2-A / LUM-1348）。
fn mount_slice_issue() -> Router<Arc<AppState>> {
    super::issues::router()
}

/// comment 切片：`/api/issues/:id/comments` + `/api/comments/:commentId*`（M2-B / LUM-1349）。
fn mount_slice_comment() -> Router<Arc<AppState>> {
    super::comments::router()
}

/// inbox 切片：`/api/inbox*`（M2-C / LUM-1350）。
fn mount_slice_inbox() -> Router<Arc<AppState>> {
    super::inbox::router()
}

/// subscriber 切片：issue 订阅/退订（M2-C / LUM-1350）。独立文件——这 4 条路径挂在
/// `/api/issues/:id` 下，若放进 M2-A 的 `issues.rs` 会制造同文件冲突（docs/10 §2 M2-C）。
fn mount_slice_subscriber() -> Router<Arc<AppState>> {
    super::subscribers::router()
}
