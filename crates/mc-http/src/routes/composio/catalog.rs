//! composio 目录与连接面：**3 条**路由（`router.go:1867/1868/1869`）—— 写者 **M8-6**。
//!
//! - `/api/integrations/composio/toolkits`（GET）：toolkit 目录，**auth-config 未配置的
//!   toolkit 不出现**（M8-6 的 `DoD`）。
//! - `/api/integrations/composio/connections`（GET）：当前用户的连接列表。
//! - `/api/integrations/composio/connections/:id`（DELETE）：断开一条连接。
//! - **未配置**：四种条件缺一 ⇒ 503；匿名 ⇒ 401。
//! - **本文件写者**：M8-6（anchor 期是**空** `Router::new()`）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
