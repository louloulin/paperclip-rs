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
            "/api/skills",
            get(health::placeholder).post(health::placeholder),
        )
        .route(
            "/api/plugins",
            get(health::placeholder).post(health::placeholder),
        )
        // TODO(LUM-1563 第 2 笔提交): 删掉下面这条 M0 占位（它由下一笔提交连同 ⑦ 基线刷新 /
        // allowlist 删行一起预删，理由见文件末尾 M5 anchor 段落）。
        .route(
            "/api/autopilots",
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
        // ----- M4 切片占位（anchor scaffold 已接好，切片只需填自己的 router） -----
        .merge(mount_slice_project())
        .merge(mount_slice_squad())
        .merge(mount_slice_chat())
        // ----- M5 切片占位（anchor scaffold 已接好，切片只需填自己的 router） -----
        .merge(mount_slice_autopilot())
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
// ✅ 两条 M0 占位（`/api/agents`、`/api/runtimes`）已由 **W3b anchor 预删**删除
// （LUM-1435 的 03:30 cycle，配方与实测见 docs/36-M3-W3B-PREFLIGHT.md §3）：
// 它们分别属于 M3-5 / M3-4，两块只隔 4 行 ⇒ 留给切片删就会有两片同改本文件 +
// ⑦ 基线 + ⑨ 快照（三个共享文件、两个写者）。预删落地后 ⑦ 基线 140→136、⑨ 快照
// 同步重生成，M3-4 / M3-5 的写集里不再出现 `mount.rs` / ⑦ 基线 / ⑨ 快照。
// 注：切片接线时必须删整块 M0 占位，否则 axum 0.7 同 path+method 重复注册
// 会在启动时 panic（docs/15 §9.6.6）。

/// agent 切片：`/api/agents*`（M3-5）。M0 占位已由 W3b anchor 预删（docs/36 §3），
/// 切片只需在此实现 `router()`，不必再动本文件。
fn mount_slice_agent() -> Router<Arc<AppState>> {
    super::agents::router()
}

/// runtime 切片：`/api/runtimes*` + runtime-profile 台账（M3-4）。M0 占位已由 W3b
/// anchor 预删（docs/36 §3），切片只需在此实现 `router()`，不必再动本文件。
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

// ---------------------------------------------------------------------------
// M4 anchor scaffold（LUM-1470 / docs/42-M4-PLAN.md §5.2–§5.3）
// ---------------------------------------------------------------------------
//
// 三个空切片先接好，M4 的四个面（M4-1 project / M4-2 squad / M4-3 + M4-4 chat）
// 各自只实作自己的 `routes/*.rs`，不再分别改本文件。stub router 目前是空
// `Router::new()`，因此合并本片后**路由表只剩下面 6 行的删除**（route_parity 基线
// 已随之刷新）。
//
// ✅ 三条 M0 占位（`GET|POST /api/chat/sessions`、`GET|POST /api/squads`、
// `GET|POST /api/projects`，共 6 个注册键）已由 **M4-0 anchor 预删**删除
// （LUM-1470，配方见 docs/42 §5.2）：它们与上游的真形态**不是同一个注册键**
// （占位是无尾斜杠的 `/api/chat/sessions`，上游 `router.go:2335-2336` 的
// `Route("/api/chat/sessions") + Post("/") / Get("/")` 服务的是带尾斜杠形态），
// 而 chi 的 `Mount` 两种形态都服务 ⇒ 留着不仅会永久留下 501 幽灵路由，还会被门 ⑦
// 的 `slash_aliases()` 折叠算成「已实现」，从报表上看不出来（docs/42 §1.1 形态纪律）。
// 预删后 ⑦ 基线 248→242、`slash-alias-allowlist.tsv` 同步删掉 6 行（否则判 STALE）。
// 三个面共用同一段代码位置（L49/L65/L69）⇒ 留给切片删就是三片同改本文件 +
// ⑦ 基线 + ⑨ 快照（三个共享文件、三个写者），这是本 anchor 存在的理由。
// 注：切片接线时**不得**再加无尾斜杠形态的同键路由——axum 0.7 同 path+method
// 重复注册会在启动时 panic（docs/15 §9.6.6），且 chi 的两种形态**都要**注册
// （见 `routes/{projects,squads}.rs` / `routes/chat/mod.rs` 的模块文档）。

/// project 切片：`/api/projects*`（M4-1）。M0 占位已由 M4-0 anchor 预删（docs/42 §5.2），
/// 切片只需在此实现 `router()`，不必再动本文件。
fn mount_slice_project() -> Router<Arc<AppState>> {
    super::projects::router()
}

/// squad 切片：`/api/squads*`（M4-2）。M0 占位已由 M4-0 anchor 预删（docs/42 §5.2），
/// 切片只需在此实现 `router()`，不必再动本文件。
fn mount_slice_squad() -> Router<Arc<AppState>> {
    super::squads::router()
}

/// chat 切片：`/api/chat*`（M4-3 会话/消息读面/快捷栏 + M4-4 派发与生成面）。
/// M0 占位已由 M4-0 anchor 预删（docs/42 §5.2）。本函数只合并 `routes/chat/mod.rs`
/// 的聚合 router，后者再合并 4 个子模块 ⇒ M4-3 / M4-4 只写各自的子文件，
/// **都不改本文件**。
fn mount_slice_chat() -> Router<Arc<AppState>> {
    super::chat::router()
}

// ---------------------------------------------------------------------------
// M5 anchor scaffold（LUM-1563 / docs/44-M5-PLAN.md §3.1）
// ---------------------------------------------------------------------------
//
// 三个面（autopilot 20 条 / issue wakeup 8 条 / webhook 1 条）先接好，M5 的六个实现切片
// 各自只实作自己的 `routes/*.rs`，不再分别改本文件。合并本片后**注册键集合只少 2 个**
// （下面那两条 M0 占位）：
// - `super::autopilots::router()` / `super::webhooks::router()` 的子文件目前是**空**
//   `Router::new()` ⇒ 不加任何注册键；
// - `super::issue_wakeups::router()` 与 `issues::wakeups::router()` 是**逐字搬运**的 501
//   占位（从 `issues/mod.rs` 搬出，7 个注册键**一个不少**）⇒ ⑦ 的 `local` 从 292 掉到 290，
//   与 `docs/44` §6.1 的预测表一致。
//
// ✅ 两条 M0 占位（`GET|POST /api/autopilots`）已由 **M5-0 anchor 预删**：它们与上游的真形态
// **不是同一个注册键**（占位是无尾斜杠的 `/api/autopilots`，上游 `router.go:2101-2102` 的
// `Route("/api/autopilots") + Get("/") / Post("/")` 服务的是带尾斜杠形态），而 chi 的
// `Mount` 两种形态都服务 ⇒ 留着不仅会永久留下 501 幽灵路由，还会被门 ⑦ 的
// `slash_aliases()` 折叠算成「已实现」，从报表上看不出来（docs/44 §6.1 的 292→290 就是这两条）。
// 预删后 `docs/fixtures/slash-alias-allowlist.tsv` 同步删掉那 2 行（否则判 STALE）。
// 注：切片接线时**必须**照上游注册**两个**形态（`/api/autopilots` + `/api/autopilots/`，
// 方法集合逐字相同），漏一个会被 `slash_alias_audit.py` 判 `MISSING_ALIAS` —— 这条**不再有
// allowlist 退路**。另：`issue_wakeups.rs` / `issues/wakeups.rs` 里的 handler 名必须仍是
// `not_implemented`（门 ⑦ 的占位正则只认 `\bplaceholder\b`，改名等于偷改门禁语义，见 R3）。

/// autopilot 面切片：`/api/autopilots*`（M5-1..M5-4）+ `/api/webhooks*`（M5-5）+
/// `/api/issue-wakeups`（M5-6）—— 三个 M5 owned 的面全在这一个 mount 函数里，
/// 所以 M5 的任何切片都不再需要改本文件（与 M4-0 同手法）。
fn mount_slice_autopilot() -> Router<Arc<AppState>> {
    Router::new()
        .merge(super::autopilots::router())
        .merge(super::webhooks::router())
        .merge(super::issue_wakeups::router())
}
