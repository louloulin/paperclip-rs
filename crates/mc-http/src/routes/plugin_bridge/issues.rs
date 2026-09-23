//! bridge 面 `/api/plugin-bridge/v1/issues*`（**4 个注册键**）。
//!
//! - **写者**：M6-7（`docs/57` §3.2）。
//! - **上游**：`router.go:104-107`（与 `/v1/issues*` **同一组 handler**）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/plugin-bridge/v1/issues/:issue_ref` | GET, PATCH | `router.go:104-105` |
//! | `/api/plugin-bridge/v1/issues/:issue_ref/comments` | GET, POST | `router.go:106-107` |
//!
//! - **实现不在本文件**：在 `routes/v1/issues.rs` 的 `pub(crate)` 实现里；本文件只挂到 bridge 前缀。
//! - `:issue_ref` 是**不透明引用**（可能是 `owner/repo#123` 形态）：按 `String` 收，
//!   别用 `Uuid` 提取器。
//! - **不做什么**：不做凭据校验（挂载点见 `plugin_bridge/mod.rs`）。
//!
//! **状态：M6-7 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 110 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/plugin-bridge/v1/issues*`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
