//! `POST /api/integrations/composio/connect/init`（`router.go:1866`）—— 写者 **M8-6**。
//!
//! - **授权层**：Auth 组内（匿名 ⇒ 401）。
//! - **未配置**：四种条件缺一 ⇒ 503（`docs/61` §2.5）。
//! - **本文件写者**：M8-6（anchor 期是**空** `Router::new()`）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
