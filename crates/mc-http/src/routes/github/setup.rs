//! `GET /api/github/setup`（`router.go:1491`，**公开块**）—— 写者 **M8-1**。
//!
//! - **凭据**：`GITHUB_WEBHOOK_SECRET` 签的 `state`（`<workspaceID>.<nonce>.<sigHex>`，
//!   HMAC-SHA256）。state 不合法 ⇒ 400/401；`GITHUB_APP_SLUG` 或 secret 缺失 ⇒「未配置」
//!   语义（**逐端点**，`docs/61` §2.4 / §2.5）。
//! - **形态**：只注册无尾斜杠那一形态（M8 无 allowlist 退路）。
//! - **本文件写者**：M8-1（anchor 期是**空** `Router::new()`）。
//! - **测试**：至少一条测试（handler 级或 e2e），**不用** `health::placeholder`。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
