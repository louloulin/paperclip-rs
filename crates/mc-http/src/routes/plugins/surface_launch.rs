//! surface 启动路由：签发一次性 surface 访问（**1 个注册键**）。
//!
//! - **写者**：M6-6（`docs/57` §3.2）。真正的 `/plugin-surfaces/:token` 页面在 M6-7 的
//!   `routes/surfaces.rs` —— 本文件只负责「从管理面拿到启动凭据」这一步。
//! - **上游**：`internal/handler/plugin_surface.go` 的 launch 段（+ `surfaceToken` 的签发/校验）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/workspaces/:id/plugins/:installationId/surfaces/:surfaceKey/launch` | GET | `router.go:1694` |
//!
//! - **三条硬纪律**（`docs/57` §4.2 M6-6/M6-7 的安全约束）：
//!   1. **未配置即禁用**：`MULTICA_PLUGIN_SURFACE_ORIGIN` 与部署密钥缺任何一个 ⇒
//!      503 `plugin_surfaces_not_configured`（上游 `writeFeatureDisabled` 的逐字口径）。
//!      本仓读这两个值：origin 从配置、密钥从 `state.plugin_key()`；**未配置 ⇒ 不签发、不 panic**。
//!   2. 令牌**绝不进 iframe**：surface 页面只拿短期 token，回调进插件的请求由宿主代发。
//!   3. `surfaceKey` 必须在已安装 manifest 的 `contributes.surfaces` 里（判定读
//!      `mc_plugin_host::manifest`），未知 key ⇒ 404。
//! - **不做什么**：不落库（surface token 是进程内的，不持久化）。
//!
//! **状态：M6-6 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 220 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/workspaces/:id/plugins/:installationId/surfaces/:surfaceKey/launch`（M6-6 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
