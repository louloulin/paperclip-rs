//! 插件安装生命周期路由：列表 / 安装 / 预览 / 卸载 / 配置 / 启停 / 令牌（**9 个注册键**）。
//!
//! - **写者**：M6-5（`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin.go`（+ `internal/service/plugin.go` 的 `DeploymentKey` 派生）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/workspaces/:id/plugins` | GET, POST | `router.go:1690` / `1736` |
//! | `/api/workspaces/:id/plugins/preview` | POST | `router.go:1735` |
//! | `/api/workspaces/:id/plugins/:installationId` | DELETE | `router.go:1747` |
//! | `/api/workspaces/:id/plugins/:installationId/config` | PUT | `router.go:1744` |
//! | `/api/workspaces/:id/plugins/:installationId/enable` | POST | `router.go:1745` |
//! | `/api/workspaces/:id/plugins/:installationId/disable` | POST | `router.go:1746` |
//! | `/api/workspaces/:id/plugins/:installationId/token` | POST, DELETE | `router.go:1738-1739` |
//!
//! - **部署密钥的唯一 egress**：hook 签名/加密要用部署密钥。本仓的读入口是
//!   `state.plugin_key()`（`Option<&PluginSecretKey>`，读 `MULTICA_PLUGIN_SECRET_KEY`）。
//!   未配置时**不得 panic / 不得用零密钥**：按上游口径降级成明确错误码
//!   （`plugin_disabled`；surface 面是 `plugin_surfaces_not_configured`）。
//!   ⚠️ 本文件**不要**自己读环境变量。
//! - **令牌**：明文只在签发响应里出现一次；库里只有 `token_hash`（`mpc_` 回调令牌根本不落库）。
//!   轮换要同时更新 `token_hash` + `token_rotated_at`。
//! - **不做什么**：不做包管理（`packages.rs`）、不做运行时面（`mcp.rs` / `surface_launch.rs`）。
//!
//! **状态：M6-5 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 520 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/workspaces/:id/plugins*` 的安装面（M6-5 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
