//! M5-0 anchor：`/api/issues/:id/wakeups*` 的 6 条注册 —— **从 `issues/mod.rs` 逐字搬来的 501 占位**。
//!
//! - **写者**：M5-6（`docs/44` §3.2；M5-0 只把注册点从 `issues/mod.rs` 挪过来）。
//! - **上游**：`handler/issue_wakeup.go`320（`ListIssueWakeups`31 / `CreateIssueWakeup`47 /
//!   `DisableIssueWakeup`25 / `EnableIssueWakeup`31 / `EditIssueWakeupInstruction`30）+ 子路由
//!   注册在 `router.go:1986-1991`（挂在 `/api/issues/{id}` 的 chi `Mount` 下）。
//!
//! # 为什么 M5-0 要把这 6 个 501 搬过来（而不是删掉重注册）
//!
//! 1. `issues/mod.rs` 由 M2-A 落成，**不在 M5 的写集**里 ⇒ 不搬就永远得让某一片去改它；
//! 2. 门 ⑦ 的 `local` 注册键数**必须逐字不变**：`docs/44` §6.1 的预测是 M5-0 后
//!    `local 292 → 290`（只掉那 2 个 autopilot 占位），**7 条 wakeup 501 仍要留在注册表里**
//!    （它们被 `route_parity.py` 计在 `implemented_real` 里 —— 这是 R3 登记的检测器缺陷，
//!    **本波不修**）。删掉再让 M5-6 重新注册，`local` 会先掉到 283，与预测表和真库门禁读数
//!    全对不上；
//! 3. **handler 名必须仍然是 `not_implemented`**：`route_parity.py` 的占位正则是
//!    `\bplaceholder\b`，改名会把这 7 个键从 `implemented_real` 翻成 `implemented_placeholder`，
//!    等于偷偷改了门禁语义。
//!
//! # 切片要实现什么
//!
//! 6 条路由（7 个注册键，因为 #1 是同 path 的 `GET` + `POST`）：
//!
//! | # | 方法 | 路径 | 上游 handler |
//! | ---: | --- | --- | --- |
//! | 22 | GET | `/api/issues/:id/wakeups` | `ListIssueWakeups`31 |
//! | 23 | POST | `/api/issues/:id/wakeups` | `CreateIssueWakeup`47 |
//! | 24 | PUT | `/api/issues/:id/wakeups/:wakeupId` | `CreateIssueWakeup`（upsert 复用） |
//! | 25 | POST | `/api/issues/:id/wakeups/:wakeupId/disable` | `DisableIssueWakeup`25 |
//! | 26 | POST | `/api/issues/:id/wakeups/:wakeupId/enable` | `EnableIssueWakeup`31 |
//! | 27 | PATCH | `/api/issues/:id/wakeups/:wakeupId/instruction` | `EditIssueWakeupInstruction`30 |
//!
//! **形态纪律**：上游这 6 条是 `r.Get("/wakeups")` 之类的 plain 子路由（**不是**
//! `Route(...) + Get("/")`）⇒ 上游只有**一个**形态，**不要**加尾斜杠别名（`EXTRA_ALIAS` 警告）；
//! 路径参数沿用 `:wakeupId`（M2-A 已经这么写，改了等于换注册键）。`mc_repos::wakeup` 与
//! `mc_autopilot::wakeup` 是它们的仓储/服务面（M5-6 同片）。
//!
//! 相邻但**不在本文件**的：`GET /api/issue-wakeups`（workspace 级）在 `super::super::issue_wakeups`；
//! `GET /api/issue-wakeup-summaries`（#29）本波**不注册**，保持 `known_gap`（`docs/44` §1.1）。

use axum::routing::{get, patch, post, put};
use axum::Router;
use std::sync::Arc;

use super::not_implemented;
use crate::state::AppState;

/// wakeup 子切片：M5-6 把 6 条占位换成真实 handler（注册键与 handler 名规则见文件头）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/issues/:id/wakeups",
            get(not_implemented).post(not_implemented),
        )
        .route("/api/issues/:id/wakeups/:wakeupId", put(not_implemented))
        .route(
            "/api/issues/:id/wakeups/:wakeupId/disable",
            post(not_implemented),
        )
        .route(
            "/api/issues/:id/wakeups/:wakeupId/enable",
            post(not_implemented),
        )
        .route(
            "/api/issues/:id/wakeups/:wakeupId/instruction",
            patch(not_implemented),
        )
}
