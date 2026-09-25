//! GitHub 安装与仓库浏览面：**4 条**路由（`router.go:1757/1672/1758/1759`）—— 写者 **M8-1**。
//!
//! | 注册键 | 方法 | 授权层 |
//! | --- | :-: | --- |
//! | `/api/workspaces/:id/github/connect` | GET | workspace **admin**（未配置 ⇒ 200 + `configured:false`） |
//! | `/api/workspaces/:id/github/installations` | GET | workspace **member**（未配置 ⇒ 200 + 空数组） |
//! | `/api/workspaces/:id/github/installations/:installationId/repositories` | GET | workspace **admin** |
//! | `/api/workspaces/:id/github/installations/:installationId` | DELETE | workspace **admin** |
//!
//! - **全路径注册、不 `nest`**：避免与既有 `/api/workspaces/:id` 抢挂载点（axum 0.7 会 panic）。
//! - **离线替身**：`mc_vcs_github::rest::GithubClient` 的 `api_base` 可按 `docs/61` §4.2 注入。
//! - **凭据纪律**：响应/日志不得回显 App 私钥或 installation token（`docs/61` §2.4）。
//! - **本文件写者**：M8-1（anchor 期是**空** `Router::new()`）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
