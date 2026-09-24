//! bridge 面 `/api/plugin-bridge/v1/context`（**1 个注册键**）。
//!
//! - **写者**：M6-7（`docs/57` §3.2 的 `routes/plugin_bridge/*.rs`）。
//! - **上游**：`router.go:103`（与 `/v1/context` **同一条 handler**）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/plugin-bridge/v1/context` | GET | `router.go:103` |
//!
//! - **实现不在本文件**：投影与查询在 `routes/v1/context.rs` 的 `get_context` 里，本文件只把它
//!   挂到 bridge 前缀上。**不要**在这里再写一份投影（两处会漂移；DoD 的「两侧响应字节相同」
//!   靠的就是同一函数）。
//! - **本文件只有 1 条路由**，故**不**自己加 `/v1` 的限流层（限流的挂载点在
//!   `routes/v1/mod.rs` 的合并点，见 `routes/v1/policy.rs` 的 ⚠️ 段）；但**要**加会话面的
//!   信任边界层：桥面不认插件令牌（`docs/57` §2.3 的「会话中继面」一行）。
//! - **不做什么**：不做凭据校验、不碰 `plugin_storage`（`storage.rs`）。
//!
//! **状态：M6-7 已落地。**
//!
//! 行预算（门 ⑩）：本文件 ≤120 行。

use axum::routing::get;
use axum::Router;
use std::sync::Arc;

use crate::routes::v1::{context, policy};
use crate::state::AppState;

/// `/api/plugin-bridge/v1/context`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    policy::apply_bridge(
        Router::new().route("/api/plugin-bridge/v1/context", get(context::get_context)),
    )
}
