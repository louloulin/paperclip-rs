//! agent MCP 服务器绑定：**4 条**路由（`router.go:2206/2207/2208/2209`）—— 写者 **M8-3**。
//!
//! - **授权层**：`loadAgentForUser`（agent owner 或 workspace owner/admin）。
//! - **`enabled` 开关幂等**：重复置同值不报错（M8-3 的 `DoD`）。
//! - **write-only**：同 `workspace.rs`。
//! - **本文件写者**：M8-3（anchor 期是**空** `Router::new()`）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
