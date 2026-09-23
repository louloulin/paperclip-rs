//! `/v1/context`（**1 个注册键**）+ 该 handler 的**共享实现**（bridge 面复用）。
//!
//! - **写者**：M6-7（`docs/57` §3.2）。
//! - **上游**：`router.go:103`（`GET /v1/context`）+ `internal/handler/plugin_surface.go` 的 context 段。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/v1/context` | GET | `router.go:103` |
//!
//! - **共享实现的口径**：`/api/plugin-bridge/v1/context` 是**同一条** handler 挂在另一个前缀上
//!   （上游逐字如此）。本地把实现写成 `pub(crate) async fn get_context(...)` 放在本文件，
//!   `routes/plugin_bridge/context.rs` 只做「同一个 handler 的另一个 router 切片」——
//!   这样 `Context` 的字段投影只有一份（两份会在 bridge/公开面之间漂移）。
//! - **返回内容**：调用者（插件）/工作区 /可见的 surface 等**上下文**，按凭据种类收窄；
//!   **绝不**回显密钥值（配置里只暴露键名，见 `mc-mcp::oauth` 的 `configured_secrets` 口径）。
//! - **不做什么**：不在这里做凭据校验（`policy.rs`）。
//!
//! **状态：M6-7 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 220 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/v1/context`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
