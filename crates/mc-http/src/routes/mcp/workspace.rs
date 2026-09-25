//! workspace MCP 服务器库：**4 条**路由（`router.go:1686/1710/1711/1712`）—— 写者 **M8-3**。
//!
//! - **授权层**：读 = member；写 3 条 = admin。
//! - **write-only**：响应**不得**含 `config` 的值字段（`headers` / `env`）。
//! - **重名拒绝**：由 `316` 迁移的唯一约束。
//! - **本文件写者**：M8-3（anchor 期是**空** `Router::new()`）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
