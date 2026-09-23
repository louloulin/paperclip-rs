//! `/v1/storage*`（**4 个注册键**）+ handler 共享实现（bridge 面复用）。
//!
//! - **写者**：M6-7（`docs/57` §3.2）。
//! - **上游**：`router.go:108-111`；落库走 `mc_repos::plugin::storage`（M6-7 也是它的写者）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/v1/storage/:scope` | GET | `router.go:108` |
//! | `/v1/storage/:scope/:key` | GET, PUT, DELETE | `router.go:109-111` |
//!
//! - **`:scope` 是 `workspace` / `user` 两态**（迁移 `344` 的 CHECK）：未知取值 ⇒ 400，不是 404。
//!   `scope_id` 由**凭据**决定（`user` ⇒ 调用者用户 id，`workspace` ⇒ 工作区 id）——
//!   **不要**从请求体/查询串里取 `scope_id`（那等于让调用者读写别人的键）。
//! - **配额**：1000 键 / 5 MiB（软配额、无淘汰）超限返回明确错误；键不存在时 `GET` ⇒ 404、
//!   `DELETE` 幂等（上游口径）。
//! - **共享实现**：`pub(crate)` 放本文件，`routes/plugin_bridge/storage.rs` 只挂同一个 handler。
//! - **不做什么**：不做加密（storage 是明文不透明值；密文面是 `plugin_secret`）。
//!
//! **状态：M6-7 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 300 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/v1/storage*`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
