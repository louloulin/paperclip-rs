//! bridge 面 `/api/plugin-bridge/v1/storage*`（**4 个注册键**）。
//!
//! - **写者**：M6-7（`docs/57` §3.2）。
//! - **上游**：`router.go:108-111`（与 `/v1/storage*` **同一组 handler**）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/plugin-bridge/v1/storage/:scope` | GET | `router.go:108` |
//! | `/api/plugin-bridge/v1/storage/:scope/:key` | GET, PUT, DELETE | `router.go:109-111` |
//!
//! - **实现不在本文件**：在 `routes/v1/storage.rs` 的 handler 里；本文件只挂到 bridge 前缀。
//!   落库走 `mc_repos::plugin::storage`（**同一个**仓储文件，两个前缀共用）。
//! - `:scope` ∈ `workspace` / `user`（未知 ⇒ 400）；`scope_id` 由**凭据**决定，绝不从
//!   请求体/查询串取（否则等于越权读写）。
//! - **不做什么**：不做加密、不做配额实现（配额判定在 `mc-repos` 里一次）。
//!
//! **状态：M6-7 已落地。**
//!
//! 行预算（门 ⑩）：本文件 ≤130 行。

use axum::routing::get;
use axum::Router;
use std::sync::Arc;

use crate::routes::v1::{policy, storage};
use crate::state::AppState;

/// `/api/plugin-bridge/v1/storage*`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    policy::apply_bridge(
        Router::new()
            .route(
                "/api/plugin-bridge/v1/storage/:scope",
                get(storage::list_storage_keys),
            )
            .route(
                "/api/plugin-bridge/v1/storage/:scope/:key",
                get(storage::get_storage_value)
                    .put(storage::put_storage_value)
                    .delete(storage::delete_storage_value),
            ),
    )
}
