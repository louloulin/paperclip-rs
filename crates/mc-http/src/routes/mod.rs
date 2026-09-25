//! Routes 聚合：所有 router 注册点。
//!
//! 命名约定：
//! - 每个领域模块一个 `pub mod`（auth / workspaces / members / invitations / ...）
//! - 各 sub-issue 在不修改本 mod.rs 的前提下，独立新增领域模块文件并在外层 build.rs
//!   或 `mount_*.rs` 切片里注册自己的 router
//! - 当前文件**仅保留 health + openapi + M0 占位**；M1 切片在独立的 `mount.rs`
//!   里组合各领域 router，避免多分支同时编辑本文件造成冲突

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

pub mod auth;
pub mod health;
pub mod mount;
pub mod openapi;
pub mod workspaces;

// M1 sub-issue C 添加：invitation + PAT 用户管理路由。命名刻意避开 sub-issue B
// 的 `auth` 命名空间，所以这里另起 `auth_user`（仅含 `AuthUser` 提取器）。
pub mod auth_user;
pub mod invitations;
pub mod pats;

// M1-D（LUM-1347）从 LUM-1335（`feat/multica-rs-m1`）cherry-pick 的 share-link 增量。
pub mod share_links;

// M2 anchor scaffold（M1-D / LUM-1347）：四个空切片一次性声明，让三个 M2 分支
// （issue / comment / inbox+subscriber）不再同时编辑本文件。真实实现在各切片内。
pub mod comments;
pub mod inbox;
pub mod issues;
pub mod subscribers;

// M2-D（LUM-1355）：issue table 查询面（`/api/issues/table/*` + `/api/issues/limit-usage`）。
// 由 `issues::router()` 内部 `merge`，因此 `mount.rs` 不需要改动。
pub mod issue_table;

// M3 anchor scaffold（LUM-1406 / docs/15-M3-PLAN.md §7.2.2）：四个空切片一次性声明，
// 让 W3a/W3b/W3c 的四个切片（agent / runtime-profile / task / daemon）不再同时编辑本文件。
// 真实实现在各切片内的 `routes/*.rs`，`mount.rs` 已接好 `mount_slice_*()`。
pub mod agents;
pub mod daemon;
pub mod runtimes;
pub mod tasks;

// M4 anchor scaffold（LUM-1470 / docs/42-M4-PLAN.md §5.1 第 3 项）：三个空切片（project /
// squad / chat）一次性声明，让 M4-1..M4-4 四个切片不再同时编辑本文件。`chat` 是目录切片
// （`routes/chat/{session,message,bar,task}.rs`，由 M4-3 / M4-4 分写），其 `mod.rs` 自己
// 聚合 4 个子 router。真实实现在各切片内，`mount.rs` 已接好 `mount_slice_{project,squad,chat}()`。
pub mod chat;
pub mod projects;
pub mod squads;

// M5 anchor scaffold（LUM-1563 / docs/44-M5-PLAN.md §3.1）：三个面一次性声明，让 M5-1..M5-8
// 八个切片不再同时编辑本文件。`autopilots` 与 `webhooks` 是目录切片（各自的 `mod.rs` 自己
// 聚合子 router），`issue_wakeups` 是 `GET /api/issue-wakeups` 的单文件；
// `/api/issues/:id/wakeups*` 那 6 条在 `issues/wakeups.rs`（由 `issues/mod.rs` 内部 merge，
// 所以不在本块里）。真实实现在各切片内，`mount.rs` 已接好 `mount_slice_autopilot()`。
pub mod autopilots;
pub mod issue_wakeups;
pub mod webhooks;

// M6 anchor scaffold（LUM-1665 / docs/57-M6-PLAN.md §3.1 / §5）：五个面一次性声明，让
// M6-1..M6-9 九个切片不再同时编辑本文件。
// - `skills` / `plugins` / `plugin_bridge` 是目录切片（各自的 `mod.rs` 自己聚合子 router）；
// - `v1` 是目录切片，`mod.rs` 在合并点之后套一层 `policy::apply`（anchor 期是恒等）；
// - `surfaces` 是**单文件**（`GET /plugin-surfaces/:token`，注意**不在 `/api` 前缀下**）。
// `/api/agents/{id}/skills*` 那 6 条不在本块：它们由 M6-4 在 `routes/agents.rs` 内部加
// `mod skills;` + `merge`（见 docs/32 §9 的文件→写者表）。
// 真实实现在各切片内，`mount.rs` 已接好五个 `mount_slice_*()`（anchor 期全为空
// `Router::new()` ⇒ 合并本片后**注册键只少 4 个**：下面那两条 M0 占位）。
pub mod plugin_bridge;
pub mod plugins;
pub mod skills;
pub mod surfaces;
pub mod v1;

// M2-E（LUM-1370）：标签目录 + property 定义目录。两个面各自一个独立文件；
// `/api/issues/:id/labels*` 那 3 条注册在 `issues/mod.rs` 里指向 `labels.rs` 的
// `pub(crate)` handler（注册留在原地，避免同 path+method 重复注册 panic）。
pub mod labels;
pub mod properties;

// M2-A 尾片（LUM-1691）：`/api/issue-views*`（5）+ 指派人频次（1）在 `issue_views.rs`；
// `/api/issue-view-preferences`（2）在 `issue_view_preferences.rs`（上游自成一面 + 门 ⑩ 的
// 800 行上限要求拆）；`/api/pins*`（4）在 `pins.rs`。三个文件各自只注册自己的键，
// `mount.rs` 已接好 `mount_slice_issue_view_pin()`。
pub mod issue_view_preferences;
pub mod issue_views;
pub mod pins;

// M7 anchor scaffold（LUM-1765 / docs/60-M7-PLAN.md §3.1 / §5）：**一个**面一次声明，
// 24 条渠道路由按 5 个平台分文件（`channels/{slack,telegram,dingtalk,lark,wecom}.rs`），
// 五个写者各自只写自己那一份，**都不再编辑本文件**。`channels/mod.rs` 自己聚合 5 个子
// router，`mount.rs` 已接好 `mount_slice_channel()`（anchor 期全为空 `Router::new()`
// ⇒ 合并本片后**注册键集合逐字不变**；这也是五轮里第一个不刷 ⑦ 基线的 anchor）。
//
// ⚠️ 两条只属于本波的纪律（详情见 `channels/mod.rs`）：
// 1. **全路径注册、不 nest**：18 条 workspace 级路由写完整路径 `/api/workspaces/:id/<平台>/…`，
//    避免与 `workspaces.rs` 已有的 `/api/workspaces/:id` 抢同一个挂载点；
// 2. **不实现 `group-routes`**：`GET /api/workspaces/:id/dingtalk/group-routes` 上游已退役，
//    且 `/api/agents/:id/dingtalk/groups` 那条挂在既有 agents 子路由内部 —— 都必须**保持 404**
//    或按表里的位置注册（docs/60 §1.6）。
pub mod channels;

// M8 anchor scaffold（LUM-1797 / docs/61-M8-PLAN.md §3.1 / §5）：**四个**面一次性声明，
// 让 M8-1..M8-6 六个切片不再同时编辑本文件。四个都是**目录切片**（各自的 `mod.rs` 自己
// 聚合子 router，子文件 anchor 期为空或只有一条占位搬运）：
// - `github`（7 条路由：M8-1 的 5 条 + M8-4 的 webhook 与 `issue_pr`）；
// - `vcs`（5 条：M8-2）；`mcp`（8 条：M8-3）；`composio`（5 条：M8-6）。
// `mount.rs` 已接好 `mount_slice_code_artifacts()`。
//
// ⚠️ 本 anchor 的 **1 条占位搬运**：`GET /api/issues/:id/pull-requests` 从
// `routes/issues/mod.rs` 搬到 `routes/github/issue_pr.rs`（handler 名仍是 `not_implemented`）
// ⇒ **注册键集合逐字不变**、`implemented_placeholder` 计数不变（这是 M8-0 不刷 ⑦ 基线的机制，
// `docs/61` §9.7 第 2 条）。
// 形态纪律（docs/61 §1.4 实测 `dual-form required: 0`）：M8 的 25 条**只按上游字面量注册
// 那一形态** —— 补尾斜杠 = `EXTRA_ALIAS` 缺陷，漏字面量 = `MISSING_EXACT` —— 本波**没有**
// allowlist 退路。路径参数必须写 `:name`（matchit 0.7 把 `{name}` 当字面量 ⇒ 编译通过且恒 404）。
pub mod composio;
pub mod github;
pub mod mcp;
pub mod vcs;

// M2-A 尾-补（LUM-1793）：`POST /api/issues/:id/squad-evaluated`（1 条上游键）。
// 上游把 handler 放在 `squad.go`，但这条键的本仓落点是**新文件**而不是 `issues/mod.rs`：
// 它登记在 `/api/issues/{id}` 下，而 `issues/` 目录的写者是 M2-A 主体（LUM-1348）及其后续切片
// —— 新文件 + `mount.rs` 尾部一行追加，两侧都不动别人的行。
// 上游注册是 `r.Post("/api/issues/{id}/squad-evaluated", …)`（`router.go:2097`）的 **plain**
// 形态（不是 `Route(…)+Post("/")`）⇒ 只注册无尾斜杠那一种，多注册一条就是 `EXTRA_ALIAS`。
pub mod squad_evaluations;

pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    mount::router(state)
}
