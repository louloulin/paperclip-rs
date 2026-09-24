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
//! - **实现不在本文件**：在 `routes/v1/issues.rs` 的 handler 里；本文件只挂到 bridge 前缀。
//! - `:issue_ref` 是**不透明引用**（可能是 `owner/repo#123` 形态）：按 `String` 收，
//!   别用 `Uuid` 提取器（合法引用会被判成 400）。
//! - **会话面的信任边界**：加 `policy::apply_bridge` ⇒ 插件令牌**不认**（上游
//!   `middleware.Auth` 的位置），权限来自 workspace 成员身份（判定在 handler 里）。
//! - **不做什么**：不做 `/v1` 的限流与 `PluginBearerOnly`（那是公开面的事）。
//!
//! **状态：M6-7 已落地。**
//!
//! 行预算（门 ⑩）：本文件 ≤140 行。

use axum::routing::get;
use axum::Router;
use std::sync::Arc;

use crate::routes::v1::{issues, policy};
use crate::state::AppState;

/// `/api/plugin-bridge/v1/issues*`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    policy::apply_bridge(
        Router::new()
            .route(
                "/api/plugin-bridge/v1/issues/:issue_ref",
                get(issues::get_issue).patch(issues::patch_issue),
            )
            .route(
                "/api/plugin-bridge/v1/issues/:issue_ref/comments",
                get(issues::list_comments).post(issues::create_comment),
            ),
    )
}
