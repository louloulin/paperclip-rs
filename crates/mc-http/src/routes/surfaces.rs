//! surface 页面：`GET /plugin-surfaces/:token`（**1 个注册键**）。
//!
//! - **写者**：M6-7（`docs/57` §3.2 的 `routes/surfaces.rs`）。
//! - **上游**：`router.go:1462`（+ `internal/handler/plugin_surface.go` 的页面侧）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/plugin-surfaces/:token` | GET | `router.go:1462` |
//!
//! ## ⚠️ 这条路径**不在 `/api` 前缀下**
//!
//! 上游把它挂在 server 根下（浏览器直接打开，不是 API 调用）。所以本文件的 router 由
//! `mount_slice_plugin_surface()` 合并到全局 router，与本波其它面不同前缀。路径参数写
//! `:token`（冒号形态）——`:token` 是**路径令牌**（`docs/57` §1.4 特别注明：它是 token 段，
//! 不是普通参数段，故**不**参与尾斜杠双形态的判定）。
//!
//! ## 语义
//!
//! 1. token 是**短命一次性**凭据（`mc_plugin_host::token` 的 `mpc_` 族，进程内、不落库）：
//!    无效/过期 ⇒ 404（不要区分「格式对但过期」与「格式不对」）。
//! 2. **未配置即禁用**：`MULTICA_PLUGIN_SURFACE_ORIGIN` 或部署密钥缺 ⇒ 503
//!    `plugin_surfaces_not_configured`（上游 `writeFeatureDisabled` 的逐字错误码）。
//! 3. 返回的是**页面**（HTML），不是 JSON；CSP/iframe 沙箱头按上游口径设置，插件令牌
//!    **绝不**下发进页面（见 `plugins/surface_launch.rs` 的三条纪律）。
//! - **不做什么**：不签发 token（`plugins/surface_launch.rs`）、不做 surface 的静态资源托管。
//!
//! **状态：M6-7 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 220 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/plugin-surfaces/:token`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
