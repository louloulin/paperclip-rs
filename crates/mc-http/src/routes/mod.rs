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

pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    mount::router(state)
}
