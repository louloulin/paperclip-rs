//! `GET /api/integrations/composio/callback`（`router.go:1517`，**公开块**）—— 写者 **M8-6**。
//!
//! - **凭据**：`COMPOSIO_STATE_SECRET`（或由 `JWT_SECRET` 派生）签的 `state`；**明确从会话
//!   之外取身份**。state 不合法 ⇒ **401**（**不是** 404）—— 这正是 ⑨ 唯一那条 M8 fixture
//!   断言的语义（`docs/61` §6.2）。
//! - **本文件写者**：M8-6（anchor 期是**空** `Router::new()`）。
//! - ⚠️ 公开路由**不得**挂会话 middleware（`docs/61` §2.7 第 4 条）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
