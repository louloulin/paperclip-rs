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
        //
        // ✅ 两条 M0 占位（`GET|POST /api/skills`、`GET|POST /api/plugins`，共 4 个注册键）
        // 已由 **M6-0 anchor 预删**（LUM-1665，配方见 docs/57 §5）：它们与上游的真形态
        // **不是同一个注册键**（占位的路径本身对，但上游 router.go:2234-2235 的
        // `/api/skills/` 是 chi 的 `Mount` + `Get("/")`/`Post("/")` ⇒ **两种形态都服务**，
        // 而占位只服务无尾斜杠形态）⇒ 留着不仅会永久留下 501 幽灵路由，还会被门 ⑦ 的
        // `slash_aliases()` 折叠算成「已实现」，从报表上看不出来（与 M4-0 删
        // `/api/chat/sessions` 同一理由，docs/42 §1.1 形态纪律）。
        // 预删后：④ 注册键集合减 4；`docs/fixtures/slash-alias-allowlist.tsv` 同步删掉那 2 行
        // （否则判 STALE）；⑤ ⑦ 基线随之刷新（`--write-baseline` 与删除在同一次提交里）。
        // 注：切片接线时**必须**两形态一起注册（`/api/skills` + `/api/skills/`、
        // `/api/skills/:id` + `/api/skills/:id/`），漏一个会被 `slash_alias_audit.py` 判
        // `MISSING_ALIAS` —— 这条**不再有 allowlist 退路**（M6-0 已删那 2 行）。
        //
        // ❌ 幽灵占位 `GET /api/feature-flags`（= `health::placeholder`，**恒 501**）已由
        // **M10-0 anchor 预删**（LUM-2102，配方见 docs/64 §9.3）。判据三条：
        // ① 上游 `router.go` **根本没有这条键**（`grep -c 'feature-flags'
        //    docs/fixtures/upstream-routes.tsv` = 0）—— 它是 M0 自造的「静默假成功」路由；
        // ② 它想表达的语义（UI 读 flag）由 M10-4 的 `/api/config` 的 `feature_flags` 字段
        //    **正式承担** ⇒ 留着就是第二个真相源；
        // ③ 先例一致：M4-0 删 6 个、M6-0 删 4 个自造占位。
        // 预删后：④ 注册键集合减 1（`local 474 → 473`）、`local_only 9 → 8`
        // （其中 `local_only_placeholder 2 → 1`）；它的唯一调用者就是这一行 ⇒
        // `health::placeholder` 随之成死代码，**同一个提交里一并删除**（否则门 ③
        // `-D warnings` 红）；⑤ 该键在 `docs/fixtures/route-parity-baseline.json` 里
        // ⇒ 删除与 `--write-baseline` **必须在同一次提交**（`route_parity.py:539` 的
        // `regressions = set(baseline) - live`，只删不刷基线必判红 —— 本波**唯一**一次基线破例）。
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
        .merge(mount_slice_label_property())
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
        // ----- M6 切片占位（anchor scaffold 已接好，切片只需填自己的 router） -----
        .merge(mount_slice_skill())
        .merge(mount_slice_plugin())
        .merge(mount_slice_plugin_bridge())
        .merge(mount_slice_plugin_surface())
        .merge(mount_slice_v1())
        // ----- M7 切片占位（anchor scaffold 已接好，切片只需填自己的 router） -----
        // ⚠️ 五个平台文件 anchor 期都是空 `Router::new()` ⇒ 本行**不加任何注册键**
        // （⑦ 读数与基线逐字不变，`docs/60` §6.1 的 M7-0 行）。
        .merge(mount_slice_channel())
        // ----- M8 切片占位（anchor scaffold 已接好，切片只需填自己的 router） -----
        // ⚠️ 四个子 router anchor 期：三个空（vcs/mcp/composio）+ github 里只有一条
        // **搬运**过来的 501 占位（`/api/issues/:id/pull-requests`）⇒ 合并后**注册键集合
        // 逐字不变**（`docs/61` §6.1 的 M8-0 行：`local 406` 不动，这是第二个不刷 ⑦ 基线的 anchor）。
        .merge(mount_slice_code_artifacts())
        // ----- M2-A 尾片（LUM-1691）：issue-views / preferences / pins / 指派人频次 -----
        // 12 条键（19 个注册点：`/api/issue-views`、`/api/issue-views/:id`、`/api/pins`
        // 三组各带尾斜杠别名）；转尾斜杠形态的判据与实测见 `docs/63-M2A-TAIL-ISSUE-VIEW-PIN.md` §4。
        .merge(mount_slice_issue_view_pin())
        // ----- M2-A 尾-补（LUM-1793）：`POST /api/issues/:id/squad-evaluated` -----
        // 1 条键，**只注册无尾斜杠形态**（上游 `router.go:2097` 是 plain `r.Post`）。
        // 它的 handler 写在 `routes/squad_evaluations.rs`，不改 `issues/` 目录里的任何文件。
        .merge(mount_slice_squad_evaluation())
        // ----- M10 anchor scaffold（LUM-2102 / docs/64-M10-PLAN.md §3.1） -----
        // ⚠️ anchor 期：`probes/*` 三个子 router 与 `config.rs` 都是**空** `Router::new()`
        // ⇒ 本行**不加任何注册键**（本片 ⑦ 只减 1：上面那条被预删的幽灵占位）。
        // 🔴 若 anchor 先注册 501 占位，⑦ 会把 `owners.M10` 假清零，而 ⑨ 会从 `unmounted`
        // 变 **`mismatch`**（期望 200 / 得到 501 ⇒ `mismatch 23 → 41`）—— 这是本片的红线。
        .merge(mount_slice_probes(state.clone()))
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

/// label + property 定义目录切片（M2-E / LUM-1370）：`/api/labels*` + `/api/properties*`。
///
/// 两个面共用一个 mount 点（同一片实现）；`/api/issues/:id/labels*` 那 3 条注册在
/// `issues::router()` 里，不在本函数。
fn mount_slice_label_property() -> Router<Arc<AppState>> {
    Router::new()
        .merge(super::labels::router())
        .merge(super::properties::router())
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

// ---------------------------------------------------------------------------
// M6 anchor scaffold（LUM-1665 / docs/57-M6-PLAN.md §3.1 / §5）
// ---------------------------------------------------------------------------
//
// 五个面一次性接好，M6-1..M6-9 九个切片各自只实作自己的 `routes/*.rs`，不再分别改本文件
// （与 M4-0 / M5-0 同一手法）。五个子 router 目前都是**空** `Router::new()` ⇒ 合并本片后
// **注册键只少 4 个**（下面那两条 M0 占位），路由表其余逐字不变。
//
// ✅ 两条 M0 占位（`GET|POST /api/skills`、`GET|POST /api/plugins`）已由本 anchor 预删，
// 理由见 `router()` 里的那段注释（chi 的 `Mount` 两形态 vs 占位单形态 + 门 ⑦ 的
// `slash_aliases()` 折叠会让幽灵路由从报表上看不出来）。
//
// ⚠️ 切片接线纪律（三条，与 M5-0 同款）：
// 1. 同 path+method 重复注册 ⇒ axum 在**启动时 panic**（docs/15 §9.6.6），所以占位必须
//    在切片接线**之前**删完（本 anchor 已做）；
// 2. 尾斜杠两形态都要注册（M6 有 5 个键要求，全在 M6-2；`slash-alias-allowlist.tsv` 里
//    的豁免行已删 ⇒ MISSING_ALIAS 是硬失败）；
// 3. 路径参数写 `:id`（matchit 0.7 把 `{id}` 当字面量：编译通过但恒 404）。

/// skill 面切片：`/api/skills*`（M6-2 读写 12 条 + M6-3 导入/刷新 2 条）。
/// 本函数只合并 `routes/skills/mod.rs` 的聚合 router，后者再合并 5 个子模块 ⇒
/// M6-2 / M6-3 只写各自的子文件，**都不改本文件**。
fn mount_slice_skill() -> Router<Arc<AppState>> {
    super::skills::router()
}

/// plugin 面切片：`/api/workspaces/:id/plugins*`（M6-5 生命周期/包 13 条 + M6-6 运行时面 4 条）。
/// 由 `routes/plugins/mod.rs` 聚合，M6-8 的 job 粘合也在其中（0 路由）。
fn mount_slice_plugin() -> Router<Arc<AppState>> {
    super::plugins::router()
}

/// plugin bridge 切片：`/api/plugin-bridge/v1/*`（M6-7 的 9 条 + M6-8 的 hook 回调 1 条）。
/// 与 `/v1` 是**同一组 handler 挂两个前缀**（上游 router.go:103-111），实现在 `routes/v1/*`。
fn mount_slice_plugin_bridge() -> Router<Arc<AppState>> {
    super::plugin_bridge::router()
}

/// surface 页面切片：`GET /plugin-surfaces/:token`（M6-7，**不在 `/api` 前缀下**）。
fn mount_slice_plugin_surface() -> Router<Arc<AppState>> {
    super::surfaces::router()
}

/// 公开 Action API 切片：`/v1/*`（M6-7，9 条）。
/// `routes/v1/mod.rs` 在**合并点之后**套一层 `policy::apply`（anchor 期恒等）——
/// 限流桶按 router 实例分片，子文件各挂一层会变成 3 倍配额（见 `routes/v1/mod.rs` 的 ⚠️ 段）。
fn mount_slice_v1() -> Router<Arc<AppState>> {
    super::v1::router()
}

// ---------------------------------------------------------------------------
// M7 anchor scaffold（LUM-1765 / docs/60-M7-PLAN.md §3.1 / §5）
// ---------------------------------------------------------------------------
//
// **一个**面（24 条渠道路由，5 个平台文件）一次性接好，M7-4 / M7-5 / M7-9 / M7-14 / M7-15
// 五个写者各自只实作自己的平台文件，**都不再改本文件**（与 M4-0 / M5-0 / M6-0 同一手法）。
// 五个子 router 目前都是**空** `Router::new()` ⇒ 合并本片后**注册键集合逐字不变**
// （∅ 删除、∅ 新增；与别的 anchor 不同，M7 波**没有** M0 占位可删 ⇒ 本 anchor 是五轮里
// 第一个**不刷 ⑦ 基线**的 anchor）。
//
// ⚠️ 三条接线纪律（与 M5-0 / M6-0 同款）：
// 1. 同 path+method 重复注册 ⇒ axum 在**启动时 panic**（docs/15 §9.6.6）；
// 2. 渠道面**只注册上游那一形态**（`docs/60` §1.4 实测 `dual-form required: 0`）：
//    补尾斜杠形态 = `EXTRA_ALIAS` 缺陷，漏字面量 = `MISSING_EXACT` —— M7 **没有** allowlist
//    退路（`slash-alias-allowlist.tsv` 已空）；
// 3. 路径参数写 `:name`（matchit 0.7 把 `{{name}}` 当字面量：编译通过但恒 404）。
//
// 另：`groups-routes` 之外还有一条**反向**验收（`docs/60` §1.6）：
// `GET /api/workspaces/:id/dingtalk/group-routes` 上游已退役、⑨ 里有一条 fixture 要求它 **404**
// ⇒ 本切片与 M7-9 **都不得**顺手把它补上。

/// 渠道面切片：24 条渠道路由（M7-4 slack 4 / M7-5 telegram 4 / M7-9 dingtalk 7 /
/// M7-14 lark 5 / M7-15 wecom 4）。
///
/// 本函数只合并 `routes/channels/mod.rs` 的聚合 router，后者再合并 5 个平台文件 ⇒
/// 五个写者**都不改本文件**。anchor 期五个子 router 全空 ⇒ 零注册键。
fn mount_slice_channel() -> Router<Arc<AppState>> {
    super::channels::router()
}

// ---------------------------------------------------------------------------
// M8 anchor scaffold（LUM-1797 / docs/61-M8-PLAN.md §3.1 / §5）
// ---------------------------------------------------------------------------
//
// **四个**面（github 7 条 / vcs 5 条 / mcp 8 条 / composio 5 条 = 25 条）一次性接好，
// M8-1..M8-6 六个实现切片各自只实作自己的子文件，**都不再改本文件**（与 M4-0 / M5-0 /
// M6-0 / M7-0 同一手法）。
//
// ⚠️ 本函数是把**四个独立子系统**放在一个挂载点上（与 M7 的渠道面同形）：四个聚合
// `mod.rs` 各自合并自己的子文件，所以任一子文件的写者都不改本文件。
//
// ⚠️ 合并后**注册键集合逐字不变**的原因：vcs / mcp / composio 的子 router 全是空
// `Router::new()`；github 侧只有 `issue_pr::router()` 一条 —— 而它是从
// `issues::router()` **原地搬运**过来的 501 占位（handler 名仍是 `not_implemented`）。
// ⇒ ⑦ 的 `local` 与 `implemented_placeholder` 都不变（`docs/61` §6.1 的 M8-0 行）。
//
// ⚠️ 三条接线纪律（与 M5-0 / M6-0 / M7-0 同款）：
// 1. 同 path+method 重复注册 ⇒ axum 在**启动时 panic**（docs/15 §9.6.6）；
// 2. M8 的 25 条**只按上游字面量注册那一形态**（`dual-form required: 0`，docs/61 §1.4）：
//    补尾斜杠形态 = `EXTRA_ALIAS` 缺陷；漏字面量 = `MISSING_EXACT`（两类都是硬失败，
//    M8 **没有** allowlist 退路）；
// 3. 路径参数写 `:name`（matchit 0.7 把 `{{name}}` 当字面量：编译通过但恒 404）。

/// 代码与制品面切片：`github`（M8-1 + M8-4）/ `vcs`（M8-2）/ `mcp`（M8-3）/
/// `composio`（M8-6）。
///
/// anchor 期四个聚合 router 合并后**不加任何新注册键**：vcs/mcp/composio 空，github 里
/// 只有搬运过来的 `pull-requests` 占位。
fn mount_slice_code_artifacts() -> Router<Arc<AppState>> {
    Router::new()
        .merge(super::github::router())
        .merge(super::vcs::router())
        .merge(super::mcp::router())
        .merge(super::composio::router())
}

// ---------------------------------------------------------------------------
// M2-A 尾片（LUM-1691）
// ---------------------------------------------------------------------------
//
// 12 条键一次性接好，按**上游 handler 文件**分三个新文件：
// - `issue_views.rs`（`issue_view.go`：issue-views 5 条 + `activity.go` 的指派人频次 1 条）；
// - `issue_view_preferences.rs`（`issue_view_preference.go`：2 条）；
// - `pins.rs`（`pin.go`：4 条）。
// 拆三个而不是两个是门 ⑩（单文件 800 行硬上限）的结果 —— 合在一起是 803 行。
// `routes/mod.rs` 也只多了三行 `pub mod`，既有行一个未动（写集与在飞片零交集）。
//
// ⚠️ 三条接线纪律（与前几波同款）：
// 1. 同 path+method 重复注册 ⇒ axum 在**启动时 panic**（docs/15 §9.6.6）；
// 2. 尾斜杠形态：`/api/issue-views/`、`/api/issue-views/{id}/`、`/api/pins/` 三组必须
//    **两个形态都注册**（上游 chi `Mount`），而 `/api/issue-view-preferences`、
//    `/api/assignee-frequency`、`/api/pins/reorder`、`/api/pins/{itemType}/{itemId}`
//    是 plain 注册 ⇒ **只注册无斜杠形态**（本波 `slash-alias-allowlist.tsv` 是 0 数据行，
//    `MISSING_ALIAS` / `MISSING_EXACT` 无豁免退路）；
// 3. 路径参数写 `:name`（matchit 0.7 把 `{name}` 当字面量：编译通过但恒 404）。

/// M2-A 尾片切片：`/api/issue-views*` + `/api/issue-view-preferences` +
/// `/api/assignee-frequency` + `/api/pins*`（12 条上游键）。
fn mount_slice_issue_view_pin() -> Router<Arc<AppState>> {
    Router::new()
        .merge(super::issue_views::router())
        .merge(super::issue_view_preferences::router())
        .merge(super::pins::router())
}

// ---------------------------------------------------------------------------
// M2-A 尾-补（LUM-1793）
// ---------------------------------------------------------------------------
//
// 上游 `M2-A` 线上剩下的**最后一条**键（`docs/63-M2A-TAIL-ISSUE-VIEW-PIN.md` §6 第 4 条把它
// 明确留给本片）：`POST /api/issues/:id/squad-evaluated`。
//
// 它为什么不能直接塞进 `mount_slice_issue_view_pin()`：那条键与 LUM-1691 的 12 条**不是同一片**，
// 而且两者当时都要写「`mount.rs` / `routes/mod.rs` 的同一追加段」⇒ 本片串在它**合入之后**跑
// （`dacad392`，PR #91）。历史约束已解除，这里独立一个 `mount_slice_*` 让两片的追加段
// 在 diff 里各自成块、互不重叠。
//
// ⚠️ 接线纪律（与前几波同款）：
// 1. 同 path+method 重复注册 ⇒ axum 在**启动时 panic**（docs/15 §9.6.6）；
// 2. 上游是 plain 注册 ⇒ **只注册无尾斜杠形态**（`EXTRA_ALIAS` / `MISSING_EXACT` 都是硬失败，
//    本波 `slash-alias-allowlist.tsv` 是 0 数据行，没有豁免退路）；
// 3. 路径参数写 `:id`（matchit 0.7 把 `{id}` 当字面量：编译通过但恒 404）。

/// squad leader 判决切片：`POST /api/issues/:id/squad-evaluated`（1 条上游键）。
fn mount_slice_squad_evaluation() -> Router<Arc<AppState>> {
    super::squad_evaluations::router()
}

// ---------------------------------------------------------------------------
// M10 anchor scaffold（LUM-2102 / docs/64-M10-PLAN.md §3.1 / §4.1 第 1 行 / §9.1）
// ---------------------------------------------------------------------------
//
// **A 面 5 条上游键**（`/health` / `/healthz` / `/readyz` / `/health/realtime` / `/api/config`）
// 一次性接好，M10-1..M10-4 四个实现切片各自只填自己的文件，**都不再改本文件**
// （与 M3-0 / M4-0 / M5-0 / M6-0 / M7-0 / M8-0 同一手法）。
//
// ⚠️ **为什么两个面合成一个挂载点**：`probes/`（目录切片，`probes/mod.rs` 自己聚合
// `live`/`ready`/`realtime` 三个子 router）与 `config.rs`（单文件切片）在 anchor 的写集里只有
// **一行** `.merge(...)`（docs/64 §3.1）⇒ 两者的合并都收在这里。收益：M10-4 只需填 `config.rs`、
// **不碰本文件**（它自己的写集审计第 ② 条逐字如此写）。
//
// ⚠️ 形态纪律（与 M7-0 / M8-0 同款）：这 5 条上游全是 `r.Get("/a/b", h)` 的 **plain** 注册
// （`router.go` L1399/1400/1401/1412/1478，`docs/64` §1.1）⇒ `dual-form required: 0`
// ⇒ 只注册**无尾斜杠**那一形态。补尾斜杠 = `EXTRA_ALIAS`、漏字面量 = `MISSING_EXACT`，
// 本波 `slash-alias-allowlist.tsv` 是 **0 数据行**、**没有**豁免退路。
//
// ⚠️ 三条键**在根路径**（四条 `/health*` 不在 `/api` 前缀下）⇒ 不得被任何 `/api` 子树的
// 鉴权提取器拦住（本地 `AuthUser` 按路由挂、不全局 ⇒ 天然满足，docs/64 §1.5）。
//
// ⚠️ 接线纪律 3 条：① 同 path+method 重复注册 ⇒ axum 启动时 panic；② 路径参数写 `:name`
// （matchit 0.7 把 `{name}` 当字面量：编译通过但恒 404）；③ 不得注册 501 占位（见上）。

/// ops 探针 + UI 启动配置切片：`/health`（M10-1）、`/healthz`+`/readyz`（M10-2）、
/// `/health/realtime`（M10-3）、`/api/config`（M10-4）—— 5 条上游键。
///
/// anchor 期合并后**不给注册键集合加任何一条**（四个子 router 全空）⇒ 本片 ⑦ 的 `local`
/// 只减 1（幽灵占位），`implemented_placeholder` 保持 3。
fn mount_slice_probes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .merge(super::probes::router(state.clone()))
        .merge(super::config::router(state))
}
