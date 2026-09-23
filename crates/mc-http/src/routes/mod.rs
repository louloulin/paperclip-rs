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

pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    mount::router(state)
}
