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

pub mod health;
pub mod mount;
pub mod openapi;
pub mod workspaces;

pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    mount::router(state)
}
