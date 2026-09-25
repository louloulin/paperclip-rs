//! VCS 连接管理面：**4 条**路由（`router.go:1676/1761/1763/1762`）—— 写者 **M8-2**。
//!
//! - **未配置 vs 未授权（两层）**：产品边界关 ⇒ 403/404；缺 `MULTICA_VCS_SECRET_KEY` ⇒ 503；
//!   非 owner/admin ⇒ 403（`docs/61` §2.5）。
//! - **rotate-webhook**：旧 secret 立刻失效、新 secret 立刻生效（M8-2 的 `DoD`）。
//! - **全路径注册、不 `nest`**（避免与既有 `/api/workspaces/:id` 抢挂载点）。
//! - **本文件写者**：M8-2（anchor 期是**空** `Router::new()`）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
