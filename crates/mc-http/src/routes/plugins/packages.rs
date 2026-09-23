//! 插件包管理路由：已发布包列表 / 发布 / 本地包上传 / 删版本（**4 个注册键**）。
//!
//! - **写者**：M6-5（`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin_packages.go`。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/workspaces/:id/plugins/packages` | GET, POST | `router.go:1724-1725` |
//! | `/api/workspaces/:id/plugins/packages/local` | POST | `router.go:1726` |
//! | `/api/workspaces/:id/plugins/packages/:packageId` | DELETE | `router.go:1727` |
//!
//! - **两条硬纪律**：
//!   1. **版本不可变**（`392` 的语义）：发布落库后不得改行，改版本 = 发新版本；
//!   2. `digest` / `sha256` 是**纯 hex**（`char_length = 64` 的 CHECK）—— `sha256:` 前缀只属于
//!      bundle 的**线上**形态（见 `mc_core::skill` 的 hash 口径），别写进列里。
//! - **包体校验**：zip 条目白名单 + 体积上限归 `mc_plugin_host::bundle`（纯逻辑）；本文件只做
//!   路由、多部分体解析（axum 的 `multipart` 特征已在 M6-0 打开）与落库。
//! - **不做什么**：不做安装（`install.rs` 的 `POST /plugins` 走 `package_version_id`）。
//!
//! **状态：M6-5 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 340 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/workspaces/:id/plugins/packages*`（M6-5 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
