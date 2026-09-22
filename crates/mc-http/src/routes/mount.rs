//! Router 切片聚合：各领域模块独立注册点。
//!
//! 设计目的：
//! - 每个 M1 / M2 / ... sub-issue 添加新文件 `auth.rs` / `workspaces.rs` / ...
//! - 每个 sub-issue 只在自己新增的 `mount_slice` 函数内追加 `.merge(...)`
//! - 多分支并发开发时只读不写公共 anchor，避免 3-way merge 冲突
//!
//! 公共 anchor（本文件）：仅维护一个稳定的 `Router::new()` + 健康/占位切片。

use axum::routing::get;
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
        // `/api/auth/*` 三个 M0 幽灵占位已全部删除：`/api/auth/logout` 由 M1-D
        // （LUM-1347）删除，`/api/auth/login` 与 `/api/auth/session` 由 M1-E
        // （LUM-1362）删除。
        //
        // 理由：上游 `router.go` 只有 `/auth/{send-code,verify-code,google,logout}`
        // （L1472-1475）与 `POST /api/auth/refresh`（L1632），**没有**这三条路径；
        // 它们是 M0 自造、返回 200 empty placeholder 的「静默假成功」路由——会让
        // 客户端以为登录/会话接口已实现。见 docs/17-M1-CONTRACT-GAPS.md §3。
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
        // ----- M3 切片占位（anchor scaffold 已接好，切片只需填自己的 router） -----
        .merge(mount_slice_agent())
        .merge(mount_slice_runtime())
        .merge(mount_slice_task())
        .merge(mount_slice_daemon())
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

// ---------------------------------------------------------------------------
// M3 anchor scaffold（LUM-1406 / docs/15-M3-PLAN.md §7.2.4）
// ---------------------------------------------------------------------------
//
// 四个空切片先接好，M3 的四个面各自只实作自己的 `routes/*.rs`，不再分别改本文件
// （与 M0 的 `056d2ae`、M1-D 的 `4aa275a` 同一手法）。stub router 目前是空
// `Router::new()`，因此合并本片后**路由表逐字不变**（route_parity 基线不受影响）。
//
// ⚠️ 下图两条 M0 占位（`/api/agents` L48-51、`/api/runtimes` L52-55）**本片刻意保留**：
// 删它们属 M3-5 / M3-4 的活（删一条会掉 2 条 parity，由 M3 集成 cycle 统一刷基线，
// docs/15 §7.3）；但切片合并前必须先删整块，否则 axum 0.7 同 path+method 重复注册
// 会在启动时 panic（docs/15 §9.6.6）。

/// agent 切片：`/api/agents*`（M3-5）。切片填 handler 时**同时删除**上面
/// `/api/agents` 的 M0 占位（`get().post()` 整块）。
fn mount_slice_agent() -> Router<Arc<AppState>> {
    super::agents::router()
}

/// runtime 切片：`/api/runtimes*` + runtime-profile 台账（M3-4）。切片填 handler 时
/// **同时删除**上面 `/api/runtimes` 的 M0 占位（`get().post()` 整块）。
fn mount_slice_runtime() -> Router<Arc<AppState>> {
    super::runtimes::router()
}

/// task 切片：agent-builder + task / lifecycle / usage / retry 用户面（M3-6 / LUM-1352）。
/// ⚠️ 其中 6 条已以 stub 形式注册在 `routes/issues.rs`，切片须**原地替换**而非新增
/// （axum 0.7 同 path+method 重复注册会 panic，docs/15 §9.6.2）。
fn mount_slice_task() -> Router<Arc<AppState>> {
    super::tasks::router()
}

/// daemon 切片：`/api/daemon*` + ws 服务端（M3-7）。
fn mount_slice_daemon() -> Router<Arc<AppState>> {
    super::daemon::router()
}
