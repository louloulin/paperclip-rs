//! skill 刷新路由：`POST /api/skills/:id/refresh`（**1 个注册键**）。
//!
//! - **写者**：M6-3（`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.go` 的 refresh 段（重新从**记录下来的来源**取件并覆盖正文）。
//! - **语义**：只对**导入来的** skill 有意义（人写的 skill 没有来源）；没有来源 ⇒ 400
//!   （不是 404，也不是静默 no-op —— 上游就是这么判的）。刷新**覆盖正文与支持文件**，
//!   但**不动**标签与 `agent_skill` 绑定。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/skills/:id/refresh` | POST | `router.go:2242` |
//!
//! - **实现边界**：取件复用 `import.rs` 的那套（同超时 / 同 413），**不要**在本文件再写一份
//!   HTTP 客户端逻辑；若 `import.rs` 里的取件是私有的，把它提升为 `pub(crate)` 后**从本文件
//!   调用**（本文件与 `import.rs` 同属 M6-3，改文件不违反「一文件一写者」）。
//! - **不做什么**：不做定时自动刷新（那是 autopilot 的语义，本波没有）。
//!
//! **状态：M6-3 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 180 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `POST /api/skills/:id/refresh`（M6-3 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
