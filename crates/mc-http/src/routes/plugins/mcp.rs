//! 插件运行时面路由：调用历史 + remote MCP 工具采纳（**3 个注册键**）。
//!
//! - **写者**：M6-6（`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin_mcp.go`（+ `pkg/remotemcp` 的 `tools/list`）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/workspaces/:id/plugins/:installationId/invocations` | GET | `router.go:1737` |
//! | `/api/workspaces/:id/plugins/:installationId/mcp/:hookKey/tools` | GET, PUT | `router.go:1742-1743` |
//!
//! - **两条硬语义**：
//!   1. `GET tools` 的返回是**当前远端**的工具列表（出网 → 超时要映射成明确错误，不能挂住
//!      请求）；`PUT tools` 是**采纳**动作，落 `plugin_installation.mcp_approvals` 的 JSONB；
//!   2. 采纳要与远端**逐条交叉校验**（名字 + `schema_digest`）：远端变了 ⇒ 视为**未采纳**，
//!      要求重新采纳 —— 判据在 `mc_mcp::client`，本文件不要自己比。
//! - **出网边界**：remote MCP 的 HTTP 只走 `mc_mcp`（`reqwest` 那条依赖在 `mc-mcp` 里，
//!   `mc-http` 也有 `reqwest` 但本文件不要用它去直接调 MCP）。
//! - **不做什么**：不做 surface 启动（`surface_launch.rs`）、不做 token 签发（`install.rs`）。
//!
//! **状态：M6-6 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 380 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/workspaces/:id/plugins/:installationId/{invocations,mcp/*}`（M6-6 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
