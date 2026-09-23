//! bridge 面 `/api/plugin-bridge/v1/context`（**1 个注册键**）。
//!
//! - **写者**：M6-7（`docs/57` §3.2 的 `routes/plugin_bridge/*.rs`）。
//! - **上游**：`router.go:103`（与 `/v1/context` **同一条 handler**）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/plugin-bridge/v1/context` | GET | `router.go:103` |
//!
//! - **实现不在本文件**：投影与查询在 `routes/v1/context.rs` 的 `pub(crate)` 实现里，
//!   本文件只把它挂到 bridge 前缀上。**不要**在这里再写一份投影（两处会漂移）。
//! - **本文件只有 1 条路由**，故**不**自己加 `policy` 层（限流与凭据的挂载点分别在
//!   `routes/v1/mod.rs` 与 `routes/plugin_bridge/mod.rs` 的合并点，见那两个文件的 ⚠️ 段）。
//! - **不做什么**：不做凭据校验、不碰 `plugin_storage`（`storage.rs`）。
//!
//! **状态：M6-7 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 90 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/plugin-bridge/v1/context`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
