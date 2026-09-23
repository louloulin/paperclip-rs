//! `/v1/issues*`（**4 个注册键**）+ handler 共享实现（bridge 面复用）。
//!
//! - **写者**：M6-7（`docs/57` §3.2）。
//! - **上游**：`router.go:104-107` + `internal/handler/plugin_surface.go` 的 issues 段。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/v1/issues/:issue_ref` | GET, PATCH | `router.go:104-105` |
//! | `/v1/issues/:issue_ref/comments` | GET, POST | `router.go:106-107` |
//!
//! - **`:issue_ref` 是不透明引用**（不是 uuid：可能是 `owner/repo#123` / `MUL-123` 之类的形态）
//!   ⇒ 路径段按 `String` 收再解析；用 `Uuid` 提取器会把合法引用判成 400。
//! - **共享实现**：`pub(crate) async fn …` 放本文件，`routes/plugin_bridge/issues.rs` 只挂同一个
//!   handler（两个前缀一套实现）。**不要**在两个文件里各写一遍投影（`PATCH` 的字段允许集
//!   尤其容易漂移）。
//! - **权限**：公开面（`/v1`）与 bridge 面（`/api/plugin-bridge/v1`）的**授权主体不同**
//!   （安装令牌 vs 回调令牌）⇒ 授权判定读 `policy` 层放进请求扩展的主体，**不要**在这里
//!   重新解析凭据头。
//! - **不做什么**：不在这里做 issue 的写入审计（本仓没有这张表）。
//!
//! **状态：M6-7 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 380 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/v1/issues*`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
