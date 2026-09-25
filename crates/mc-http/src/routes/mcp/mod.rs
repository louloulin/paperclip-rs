//! MCP 服务器库面聚合：**8 条**注册键（`docs/61-M8-PLAN.md` §1.1 的第 13–20 行）—— 写者 **M8-3**。
//!
//! | 注册键 | 方法 | `router.go` | 文件 |
//! | --- | :-: | ---: | --- |
//! | `/api/workspaces/:id/mcp-servers` | GET | 1686 | `workspace.rs` |
//! | `/api/workspaces/:id/mcp-servers` | POST | 1710 | `workspace.rs` |
//! | `/api/workspaces/:id/mcp-servers/:serverId` | PUT | 1711 | `workspace.rs` |
//! | `/api/workspaces/:id/mcp-servers/:serverId` | DELETE | 1712 | `workspace.rs` |
//! | `/api/agents/:id/mcp-servers` | GET | 2206 | `agent.rs` |
//! | `/api/agents/:id/mcp-servers` | POST | 2207 | `agent.rs` |
//! | `/api/agents/:id/mcp-servers/:serverId/enabled` | PUT | 2208 | `agent.rs` |
//! | `/api/agents/:id/mcp-servers/:serverId` | DELETE | 2209 | `agent.rs` |
//!
//! - **授权层**：workspace 面 = **member** 读 / **admin** 写 3 条；agent 面 4 条走
//!   `loadAgentForUser`（agent owner 或 workspace owner/admin，与 M6-4 的 `/skills*` 同手法）。
//! - **无「未配置」语义**：这是**本地库**，不依赖外部密钥 —— 只有授权语义（`docs/61` §2.5）。
//! - **write-only 硬约束**：响应 DTO **不含** `headers` / `env` 的**值**字段（`docs/61` §2.7 第 5 条）。
//! - **不得重复实现**（`docs/61` §2.3）：不与 `mc-mcp`（remote MCP 客户端）、
//!   `mc-repos/src/plugin/mcp_approval`（插件远程 MCP）、`mc-daemon/src/mcp/**`（daemon 运行时
//!   MCP）混同。
//! - **anchor 期**：两个子文件全是**空** `Router::new()` ⇒ 合并后**零注册键**。

pub mod agent;
pub mod workspace;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// MCP 面的聚合 router（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(workspace::router())
        .merge(agent::router())
}
