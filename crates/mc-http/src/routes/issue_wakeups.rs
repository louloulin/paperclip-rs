//! M5-0 anchor：`GET /api/issue-wakeups`（workspace 级 wakeup 列表）—— **从 `issues/mod.rs`
//! 逐字搬来的 501 占位**。
//!
//! - **写者**：M5-6（`docs/44` §3.2；M5-0 只把注册点挪过来）。
//! - **上游**：`ListWorkspaceWakeups`81（`handler/issue_wakeup.go`），注册在 `router.go:2319-2320`
//!   的 workspace 级两条之一。
//! - **为什么独立文件**：它**不**挂在 `/api/issues/{id}` 下（虽然 handler 同源），
//!   §3.2 把它和 `issues/wakeups.rs` 一起判给 M5-6，两者同片但分文件 ⇒ 避免撞门 ⑩、
//!   也避免 M5-6 去改 `issues/mod.rs`（后者不在 M5 的写集里）。
//!
//! # 与 `issues/wakeups.rs` 同一套约束（必读）
//!
//! 1）注册键数**逐字不变**：`docs/44` §6.1 预测 M5-0 后 `local 292 → 290`，只掉 2 个 autopilot 占位。
//!
//! 2）handler 名**必须仍是 `not_implemented`**：`route_parity.py` 的占位正则只认 `\bplaceholder\b`，
//! 改名会把这几个键从 `implemented_real` 翻成 `implemented_placeholder`，等于偷改门禁语义
//! （检测器缺陷登记在 R3，**本波不修**）。
//!
//! 3）单形态：上游是 plain 路由 ⇒ 不要加尾斜杠别名。
//!
//! **本波不注册**的邻居：`GET /api/issue-wakeup-summaries`（`ListWorkspaceWakeupSummaries`27，
//! 上游 `router.go:2320`）保持 `known_gap` —— M5-1..M5-5 落地后 ⑦ 的 `local` 是 319
//! （含这 1 条 summaries），本片不动它（`docs/44` §1.1 的 29 条路由里 #29 由 M5-6 决定）。

use axum::routing::get;
use axum::Router;
use std::sync::Arc;

use super::issues::not_implemented;
use crate::state::AppState;

/// workspace 级 wakeup 子切片：M5-6 把占位换成真实 handler。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/issue-wakeups", get(not_implemented))
}
