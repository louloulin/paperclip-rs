//! onboarding 面聚合：**5 条**注册键分两个子文件（`docs/62-M9-PLAN.md` §4.1 的第 4 行）。
//!
//! ## ⚠️ 本文件由 M9-0 anchor（`LUM-1815`）冻结，M9 后续切片**不得**编辑
//!
//! | 文件 | 注册键 | 写者 | 上游 |
//! | --- | :-: | :-: | --- |
//! | `onboarding/profile.rs` | 1（`PATCH /api/me/onboarding`） | **M9-3** | `onboarding.go` `PatchOnboarding` |
//! | `onboarding/shim.rs` | 2（两条 DEPRECATED 活路由） | **M9-3** | `onboarding_shim.go`（623 行） |
//! | `onboarding/cloud_waitlist.rs` | 1 | **M9-3** | `onboarding.go` `JoinCloudWaitlist` |
//!
//! 账：1 + 2 + 1 = **4**；本波 onboarding 面的第 5 条是 **`GET /api/me`**（`POST` 完成那条
//! 是 `POST /api/me/onboarding/complete`）—— 逐一对照 `docs/fixtures/upstream-routes.tsv`
//! 的 M9 段：`PATCH /api/me/onboarding`、`POST /api/me/onboarding/cloud-waitlist`、
//! `POST /api/me/onboarding/complete`、`POST /api/me/onboarding/no-runtime-bootstrap`、
//! `POST /api/me/onboarding/runtime-bootstrap` = **5 条**。
//! ⚠️ `POST /api/me/onboarding/complete` **不在本目录的三个文件里**：它挂在既有
//! `routes/workspaces.rs` 的 `/api/me` 子树上（上游也是 `r.Post("/complete", …)` 挂在
//! `/api/me/onboarding` 的 `Route` 里）⇒ **M9-3 的写集包含 `routes/workspaces.rs` 的
//! 一个追加段**，anchor 不碰它（那会与 M1-A 面抢文件）。
//!
//! ## 锚点期**零注册键**
//!
//! 三个子文件现在都是**空** `Router::new()` ⇒ `mount_slice_commercial()` 合并后
//! **注册键集合逐字不变**（`docs/62` §6.1 的 M9-0 行）。
//!
//! ## 与既有 onboarding 实现的交集（**只读**清单，`docs/62` §9.7 的表）
//!
//! `crates/mc-chat/src/onboarding.rs`（M4-4 的语言白名单与开场白）、
//! `crates/mc-repos/src/chat_task/onboarding.rs`（M4-4 的 `start_mika_onboarding`）、
//! `crates/mc-http/src/routes/chat/task/dispatch.rs`、`crates/mc-repos/src/user.rs`
//! —— 四处**一字不改**（M9-7 只**复用**它们的"取或建会话"语义）。

pub mod cloud_waitlist;
pub mod profile;
pub mod shim;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// onboarding 面的聚合 router（anchor 期 = 三次空 merge）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(profile::router())
        .merge(shim::router())
        .merge(cloud_waitlist::router())
}
