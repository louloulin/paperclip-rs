//! `POST /api/webhooks/github`（`router.go:1490`，**公开块**）—— 写者 **M8-4**。
//!
//! - **凭据**：`GITHUB_WEBHOOK_SECRET` + `X-Hub-Signature-256`（HMAC-SHA256）；缺密钥 /
//!   验签失败 ⇒ **401**（**不是** 404：路由必须在，鉴权在 handler 内，`docs/61` §1.5）。
//! - **三族事件**：`installation` / `pull_request` / `check_suite`（M8-4 的 `DoD`）。
//! - **幂等**：同一 webhook 重投 2 次只插 1 行 PR。
//! - **本文件写者**：M8-4（anchor 期是**空** `Router::new()`）。
//! - ⚠️ 公开路由**不得**挂会话 middleware，也**不得**因为缺会话就返回 401
//!   （`docs/61` §2.7 第 4 条）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
